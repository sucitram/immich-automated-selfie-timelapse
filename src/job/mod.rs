//! Background job processing module.
//!
//! Handles the complete pipeline:
//! 1. Fetching assets from Immich for a specific person
//! 2. Downloading and processing images in parallel
//! 3. Processing images through the extensible pipeline
//! 4. Compiling processed images into a timelapse video

mod processing;

use crate::config::{PersistableConfig, TimeIntervalConfig, TimeRange};
use crate::error::{Error, Result};
use crate::immich_api::{Asset, FaceData, ImmichClient};
use crate::pipeline::Pipeline;
use crate::utils::make_job_slug;
use crate::video::compile_timelapse;
use crate::web::{AppState, AtomicSkipStats, JobStatus, Progress};

use processing::{process_single_asset, AssetProcessResult, DebugDirs, OutputDirs};

use chrono::NaiveDate;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Tracks per-time-bucket photo counts for limiting.
///
/// Thread-safe: uses atomic counters with fetch_add + undo pattern
/// so concurrent workers can claim slots without races.
pub struct TimeIntervalTracker {
    max_photos: u32,
    time_range: TimeRange,
    buckets: RwLock<HashMap<String, Arc<AtomicU32>>>,
}

impl TimeIntervalTracker {
    /// Create a tracker if photo limiting is enabled, otherwise return None.
    pub fn new(config: &TimeIntervalConfig) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        Some(Self {
            max_photos: config.max_photos,
            time_range: config.time_range,
            buckets: RwLock::new(HashMap::new()),
        })
    }

    /// Try to claim a slot for the given timestamp.
    /// Returns true if the slot was claimed, false if the bucket is full.
    pub fn try_claim(&self, timestamp: &str) -> bool {
        let key = self.bucket_key(timestamp);

        // Get or create the atomic counter for this bucket
        let counter = {
            // Try read lock first
            let buckets = self.buckets.read().expect("bucket lock poisoned");
            if let Some(counter) = buckets.get(&key) {
                counter.clone()
            } else {
                drop(buckets);
                let mut buckets = self.buckets.write().expect("bucket lock poisoned");
                buckets
                    .entry(key)
                    .or_insert_with(|| Arc::new(AtomicU32::new(0)))
                    .clone()
            }
        };

        // Atomically try to claim: increment, check, undo if over limit
        let prev = counter.fetch_add(1, Ordering::SeqCst);
        if prev < self.max_photos {
            true
        } else {
            counter.fetch_sub(1, Ordering::SeqCst);
            false
        }
    }

    /// Check if the bucket for the given timestamp is already full.
    /// This is a non-mutating check used to skip processing early.
    pub fn is_full(&self, timestamp: &str) -> bool {
        let key = self.bucket_key(timestamp);
        let buckets = self.buckets.read().expect("bucket lock poisoned");
        if let Some(counter) = buckets.get(&key) {
            counter.load(Ordering::SeqCst) >= self.max_photos
        } else {
            false
        }
    }

    /// Compute the bucket key from a timestamp string.
    fn bucket_key(&self, timestamp: &str) -> String {
        // Parse date from ISO 8601 timestamp (e.g. "2024-01-15" or "2024-01-15T12:34:56Z")
        let date_str = timestamp.get(..10).unwrap_or(timestamp);
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").unwrap_or_default();

        match self.time_range {
            TimeRange::Day => date.format("%Y-%m-%d").to_string(),
            TimeRange::Week => {
                use chrono::Datelike;
                format!("{}-W{:02}", date.iso_week().year(), date.iso_week().week())
            }
            TimeRange::Month => date.format("%Y-%m").to_string(),
        }
    }
}

/// Parameters for starting a processing job.
#[derive(Debug, Clone)]
pub struct JobParams {
    pub person_id: String,
    pub person_name: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub album_ids: Vec<String>,
    pub album_names: Vec<String>,
    /// When true, delete and recreate the output directory even if it already exists.
    /// When false (default), reuse the existing directory and skip assets whose output
    /// files are already present.
    pub force_rerun: bool,
}

/// Serialisable record written to `job_manifest.json` inside each job directory.
#[derive(serde::Serialize)]
struct JobManifest<'a> {
    person_id: &'a str,
    person_name: Option<&'a str>,
    date_from: Option<&'a str>,
    date_to: Option<&'a str>,
    album_ids: &'a [String],
    album_names: &'a [String],
    force_rerun: bool,
    run_at: String,
    config: PersistableConfig,
}

/// Run the complete processing pipeline.
///
/// This is the main entry point for background job processing.
/// It handles progress reporting and cancellation.
pub async fn run_job(state: AppState, params: JobParams, cancel_token: CancellationToken) {
    let result = run_job_inner(state.clone(), params, cancel_token).await;

    // Clear the cancellation token when done
    state.clear_cancel_token().await;

    // Get final state from progress (skip stats and person info)
    let final_progress = state.progress.read().await.clone();

    match result {
        Ok(output_path) => {
            state
                .update_progress(Progress {
                    status: JobStatus::Completed,
                    message: Some(format!(
                        "Video saved to: {}\
                        <br><i>Please note that for the best \
                        looking results, you should do a manual pass in the gallery view \
                        (button available in the section below)</i>",
                        output_path.display()
                    )),
                    ..final_progress
                })
                .await;
        }
        Err(Error::Cancelled) => {
            state
                .update_progress(Progress {
                    status: JobStatus::Cancelled,
                    message: Some("Job cancelled by user".to_string()),
                    ..final_progress
                })
                .await;
        }
        Err(e) => {
            tracing::error!("Job failed: {}", e);
            let friendly = e.user_message();
            state
                .update_progress(Progress {
                    status: JobStatus::Error(friendly.clone()),
                    message: Some(friendly),
                    ..final_progress
                })
                .await;
        }
    }
}

/// Inner implementation that returns Result for easier error handling.
async fn run_job_inner(
    state: AppState,
    params: JobParams,
    cancel_token: CancellationToken,
) -> Result<PathBuf> {
    let config = state.config.read().await.clone();

    // Build a slug that encodes person + date range + album selection so that
    // multiple jobs for the same person coexist on disk.
    let folder_name = make_job_slug(
        params.person_name.as_deref(),
        &params.person_id,
        params.date_from.as_deref(),
        params.date_to.as_deref(),
        &params.album_names,
    );
    let job_dir = config.output_dir.join(&folder_name);

    if params.force_rerun && job_dir.exists() {
        tracing::info!("force_rerun: removing existing folder {}", job_dir.display());
        tokio::fs::remove_dir_all(&job_dir).await?;
    }

    // Create output directories (idempotent — create_dir_all is a no-op if they exist)
    let images_dir = job_dir.join("images");
    tokio::fs::create_dir_all(&images_dir)
        .await
        .map_err(|e| permission_aware_io_error(e, &images_dir))?;

    // Create debug directories if enabled
    let debug = if config.processing.output.keep_intermediates {
        let debug_base = job_dir.join("debug");
        tokio::fs::create_dir_all(&debug_base).await?;
        Some(DebugDirs { base: debug_base })
    } else {
        None
    };

    let video_filename = format!("{}.mp4", folder_name);
    let output_dirs = OutputDirs {
        images: images_dir.clone(),
        video: job_dir.join(&video_filename),
        debug,
    };

    tracing::info!("Output directory: {}", job_dir.display());

    // Write manifest so the folder is self-documenting.
    let manifest = JobManifest {
        person_id: &params.person_id,
        person_name: params.person_name.as_deref(),
        date_from: params.date_from.as_deref(),
        date_to: params.date_to.as_deref(),
        album_ids: &params.album_ids,
        album_names: &params.album_names,
        force_rerun: params.force_rerun,
        run_at: chrono::Utc::now().to_rfc3339(),
        config: PersistableConfig::from(&config),
    };
    if let Ok(json) = serde_json::to_string_pretty(&manifest) {
        let _ = tokio::fs::write(job_dir.join("job_manifest.json"), json).await;
    }

    // Collect already-processed asset IDs from the images directory so we can
    // skip them when force_rerun is false.  Filenames are "{timestamp}_{asset_id}.jpg".
    let already_done: HashSet<String> = if !params.force_rerun {
        let mut ids = HashSet::new();
        if let Ok(mut entries) = tokio::fs::read_dir(&images_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".jpg") {
                    // The asset_id is appended last, separated by '_'.
                    // UUIDs contain only hex and hyphens (no underscores), so
                    // splitting on '_' and taking the last token is unambiguous.
                    if let Some(id) = name.trim_end_matches(".jpg").split('_').last() {
                        ids.insert(id.to_string());
                    }
                }
            }
        }
        if !ids.is_empty() {
            tracing::info!("{} assets already processed, will skip them", ids.len());
        }
        ids
    } else {
        HashSet::new()
    };

    // Create Immich client
    let client = ImmichClient::new(&config.api)?;

    // Base progress carrying job identity, reused for all updates in this job
    let params_base = Progress {
        person_id: Some(params.person_id.clone()),
        person_name: params.person_name.clone(),
        date_from: params.date_from.clone(),
        date_to: params.date_to.clone(),
        album_ids: params.album_ids.clone(),
        album_names: params.album_names.clone(),
        ..Progress::default()
    };

    // Update progress: fetching assets
    state
        .update_progress(Progress {
            status: JobStatus::Running,
            message: Some("Fetching assets from Immich...".to_string()),
            ..params_base.clone()
        })
        .await;

    // Check for cancellation
    if cancel_token.is_cancelled() {
        return Err(Error::Cancelled);
    }

    // Fetch all assets for this person (optionally filtered by albums)
    let assets = client
        .get_assets_with_person(
            &params.person_id,
            params.date_from.as_deref(),
            params.date_to.as_deref(),
            &params.album_ids,
        )
        .await?;

    if assets.is_empty() {
        let msg = if params.album_ids.is_empty() {
            "No assets found for this person".to_string()
        } else {
            let names = if params.album_names.is_empty() {
                "selected albums".to_string()
            } else {
                params.album_names.join(", ")
            };
            format!("No assets found for this person in: {}", names)
        };
        return Err(Error::ImageProcessing(msg));
    }

    tracing::info!("Found {} assets to process", assets.len());

    // Filter assets that have face data for the target person
    let assets_with_faces: Vec<(Asset, FaceData)> = assets
        .into_iter()
        .filter_map(|asset| {
            let face = find_face_for_person(&asset, &params.person_id)?;
            Some((asset, face))
        })
        .collect();

    if assets_with_faces.is_empty() {
        return Err(Error::ImageProcessing(
            "No assets with face data found".to_string(),
        ));
    }

    // Partition into assets that need processing vs. those already done.
    let (pending, skipped_count) = if already_done.is_empty() {
        (assets_with_faces, 0usize)
    } else {
        let mut pending = Vec::new();
        let mut skipped = 0usize;
        for item in assets_with_faces {
            if already_done.contains(&item.0.id) {
                skipped += 1;
            } else {
                pending.push(item);
            }
        }
        (pending, skipped)
    };

    let total = (pending.len() + skipped_count) as u32;
    tracing::info!(
        "{} assets have face data ({} pending, {} already done)",
        total,
        pending.len(),
        skipped_count
    );

    let resume_msg = if skipped_count > 0 {
        format!(
            "Resuming: {} already done, processing {} more...",
            skipped_count,
            pending.len()
        )
    } else {
        format!("Processing {} images...", total)
    };

    // Update progress with total count (pre-credit already-done assets)
    state
        .update_progress(Progress {
            status: JobStatus::Running,
            completed: skipped_count as u32,
            total,
            message: Some(resume_msg),
            ..params_base.clone()
        })
        .await;

    // Create the processing pipeline based on config
    let pipeline = Arc::new(Pipeline::with_steps_from_config(&config));
    tracing::debug!("Pipeline steps: {:?}", pipeline.step_ids());

    // Create photo limit tracker if enabled
    let time_interval_tracker =
        TimeIntervalTracker::new(&config.processing.time_interval).map(Arc::new);

    // Process images in parallel with concurrency limit
    let completed = Arc::new(AtomicU32::new(skipped_count as u32));
    let semaphore = Arc::new(Semaphore::new(config.processing.max_workers));
    let client = Arc::new(client);
    let config = Arc::new(config);
    let output_dirs = Arc::new(output_dirs);

    // Atomic counters for real-time skip statistics (consolidated into single struct)
    let skip_stats = Arc::new(AtomicSkipStats::new());

    let mut handles = Vec::with_capacity(pending.len());

    for (asset, face_data) in pending {
        // Check for cancellation before spawning more tasks
        if cancel_token.is_cancelled() {
            break;
        }

        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => continue, // Semaphore closed, skip remaining tasks
        };
        let client = client.clone();
        let config = config.clone();
        let output_dirs = output_dirs.clone();
        let completed = completed.clone();
        let state = state.clone();
        let task_cancel_token = cancel_token.clone();
        let pipeline = pipeline.clone();

        // Clone skip stats for this task
        let skip_stats = skip_stats.clone();

        // Clone photo limit tracker for this task
        let time_interval_tracker = time_interval_tracker.clone();

        let task_base = params_base.clone();

        let handle = tokio::spawn(async move {
            // Check cancellation at start of task
            if task_cancel_token.is_cancelled() {
                drop(permit);
                return AssetProcessResult::Cancelled {
                    asset_id: asset.id.clone(),
                };
            }

            let result = process_single_asset(
                &client,
                &config,
                &asset,
                &face_data,
                &output_dirs,
                &task_cancel_token,
                &skip_stats,
                &pipeline,
                time_interval_tracker.as_ref(),
            )
            .await;

            // Update progress with current skip stats (only if not cancelled)
            if !task_cancel_token.is_cancelled() {
                let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                let current_skip_stats = skip_stats.snapshot();
                state
                    .update_progress(Progress {
                        status: JobStatus::Running,
                        completed: done,
                        total,
                        message: Some(format!("Processing images... ({}/{})", done, total)),
                        skip_stats: current_skip_stats,
                        ..task_base
                    })
                    .await;
            }

            drop(permit);
            result
        });

        handles.push(handle);
    }

    // Wait for all tasks to complete
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => {
                tracing::error!("Task panicked: {:?}", e);
            }
        }
    }

    // Check if we were cancelled
    if cancel_token.is_cancelled() {
        return Err(Error::Cancelled);
    }

    // Get final skip statistics from atomic counters
    let final_skip_stats = skip_stats.snapshot();

    // Count results and log errors (deduplicated)
    let mut successful = 0usize;
    let mut errors = 0usize;
    let mut first_error: Option<String> = None;
    let mut all_same_error = true;
    for result in &results {
        match result {
            AssetProcessResult::Success { .. } => successful += 1,
            AssetProcessResult::Error { error, .. } => {
                errors += 1;
                match &first_error {
                    None => first_error = Some(error.clone()),
                    Some(first) if first != error => all_same_error = false,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if let Some(ref err) = first_error {
        if all_same_error {
            tracing::error!("{} assets failed with: {}", errors, err);
        } else {
            // Log distinct errors individually (up to 3)
            let mut logged = 0;
            for result in &results {
                if let AssetProcessResult::Error { asset_id, error } = result {
                    if logged < 3 {
                        tracing::error!("Asset {} failed: {}", asset_id, error);
                        logged += 1;
                    }
                }
            }
            if errors > 3 {
                tracing::error!("... and {} more errors", errors - 3);
            }
        }
    }
    let skipped = final_skip_stats.total();

    tracing::info!(
        "Processing complete: {} successful, {} skipped, {} errors",
        successful,
        skipped,
        errors
    );

    // Update progress with final skip stats
    state
        .update_progress(Progress {
            status: JobStatus::Running,
            completed: total,
            total,
            message: Some("Processing complete, preparing video...".to_string()),
            skip_stats: final_skip_stats.clone(),
            ..params_base.clone()
        })
        .await;

    if successful == 0 {
        let detail = first_error
            .map(|e| format!(" First error: {}", e))
            .unwrap_or_default();
        return Err(Error::ImageProcessing(format!(
            "No images were successfully processed ({} errors).{}",
            errors, detail
        )));
    }

    // Check for cancellation before video compilation
    if cancel_token.is_cancelled() {
        return Err(Error::Cancelled);
    }

    // Compile video if enabled
    if config.video.enabled {
        state
            .update_progress(Progress {
                status: JobStatus::CompilingVideo,
                total: successful as u32,
                message: Some("Compiling video...".to_string()),
                skip_stats: final_skip_stats.clone(),
                ..params_base
            })
            .await;

        compile_timelapse(
            &output_dirs.images,
            &output_dirs.video,
            &config.video,
            Some(&cancel_token),
            |frame, total| {
                // Sync callback - just log progress. State updates would require
                // async context or channels, which adds complexity for minimal benefit
                // since video compilation is typically fast.
                tracing::debug!("Video progress: {}/{}", frame, total);
            },
        )
        .await?;

        Ok(output_dirs.video.clone())
    } else {
        Ok(output_dirs.images.clone())
    }
}

/// Convert a permission-denied I/O error into a Config error with Docker fix instructions.
fn permission_aware_io_error(e: std::io::Error, path: &std::path::Path) -> Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        Error::Config(format!(
            "Permission denied for '{}'. {}",
            path.display(),
            crate::error::PERMISSION_HINT
        ))
    } else {
        Error::Io(e)
    }
}

/// Find the face data for a specific person in an asset.
fn find_face_for_person(asset: &Asset, person_id: &str) -> Option<FaceData> {
    let people = asset.people.as_ref()?;

    for person in people {
        if person.id == person_id {
            if let Some(faces) = &person.faces {
                // Return the first face (Immich shouldn't detect the same person more than once per image)
                return faces.first().cloned();
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_face_for_person() {
        use crate::immich_api::PersonWithFaces;

        let asset = Asset {
            id: "asset1".to_string(),
            device_asset_id: None,
            original_file_name: None,
            file_created_at: Some("2024-01-15T10:30:00Z".to_string()),
            local_date_time: None,
            people: Some(vec![PersonWithFaces {
                id: "person1".to_string(),
                name: Some("Test Person".to_string()),
                faces: Some(vec![FaceData {
                    bounding_box_x1: 0.2,
                    bounding_box_y1: 0.1,
                    bounding_box_x2: 0.5,
                    bounding_box_y2: 0.6,
                    image_width: 1920,
                    image_height: 1080,
                }]),
            }]),
        };

        let face = find_face_for_person(&asset, "person1");
        assert!(face.is_some());

        let face = find_face_for_person(&asset, "person2");
        assert!(face.is_none());
    }
}
