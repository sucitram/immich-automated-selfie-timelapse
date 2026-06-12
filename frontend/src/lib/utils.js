/**
 * Shared utility functions for the frontend.
 * These must match backend logic where noted.
 */

/**
 * Sanitize a folder name to match backend's sanitize_folder_name logic.
 * IMPORTANT: Must exactly replicate src/utils.rs sanitize_folder_name
 *
 * @param {string} name - Person name
 * @param {string} id - Person ID (fallback)
 * @returns {string} Sanitized folder name
 */
export function sanitizeFolderName(name, id) {
  const base = name && name.trim() ? name : id;

  // Remove accents by decomposing to NFD and filtering combining marks
  const withoutAccents = base.normalize('NFD').replace(/[\u0300-\u036f]/g, '');

  // Convert to lowercase and replace unsafe characters with underscores
  let sanitized = '';
  for (const c of withoutAccents.toLowerCase()) {
    if (/^[a-z0-9\-_]$/.test(c)) {
      sanitized += c;
    } else {
      sanitized += '_';
    }
  }

  // Trim whitespace and underscores, then limit length
  sanitized = sanitized.replace(/^[\s_]+|[\s_]+$/g, '');
  return sanitized.length > 50 ? sanitized.slice(0, 50) : sanitized;
}

/**
 * Build a unique job slug matching the backend's make_job_slug in src/utils.rs.
 * IMPORTANT: Must replicate that function's logic exactly.
 *
 * @param {string|null} personName
 * @param {string} personId
 * @param {string|null} dateFrom - YYYY-MM-DD or empty
 * @param {string|null} dateTo   - YYYY-MM-DD or empty
 * @param {Array<{name:string}>} albums
 * @returns {string}
 */
export function makeJobSlug(personName, personId, dateFrom, dateTo, albums) {
  const person = sanitizeFolderName(personName, personId).slice(0, 30);

  const from = dateFrom && dateFrom.trim() ? dateFrom.slice(0, 10).replace(/[^a-zA-Z0-9-]/g, '') : null;
  const to   = dateTo   && dateTo.trim()   ? dateTo.slice(0, 10).replace(/[^a-zA-Z0-9-]/g, '') : null;

  let dates;
  if (!from && !to) dates = 'all';
  else if (from && !to) dates = `from_${from}`;
  else if (!from && to) dates = `to_${to}`;
  else dates = `${from}_${to}`;

  const albumPart = _albumSlug(albums);
  const slug = albumPart ? `${person}_${dates}_${albumPart}` : `${person}_${dates}`;
  return slug.slice(0, 80);
}

function _albumSlug(albums) {
  if (!albums || albums.length === 0) return null;
  if (albums.length === 1) {
    return sanitizeFolderName(albums[0].name, 'album').slice(0, 20);
  }
  if (albums.length === 2) {
    const a = sanitizeFolderName(albums[0].name, 'album').slice(0, 10);
    const b = sanitizeFolderName(albums[1].name, 'album').slice(0, 10);
    const combined = `${a}_${b}`;
    return combined.length <= 25 ? combined : `${albums.length}_albums`;
  }
  return `${albums.length}_albums`;
}

/**
 * Format bytes to human-readable size string.
 *
 * @param {number} bytes - Size in bytes
 * @returns {string} Formatted size (e.g., "1.2 MB")
 */
export function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
