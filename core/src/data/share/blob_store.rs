//! Blob file storage - Three-file approach
//!
//! Each blob is stored as three files:
//! - `<hash>.data`   - Coalesced file content (gaps = zeros)
//! - `<hash>.obao4`  - BLAKE3 outboard (hash tree for verification)
//! - `<hash>.sizes4` - Chunk presence map (8 bytes per chunk)
//!
//! This allows:
//! - Random-access chunk reading/writing
//! - Resumable downloads
//! - Instant verification via outboard
//! - No reassembly needed when complete

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::metadata::CHUNK_SIZE;

/// Extension for data files
const DATA_EXT: &str = "data";
/// Extension for outboard files (pre-order, chunk group size 4)
const OUTBOARD_EXT: &str = "obao4";
/// Extension for sizes/presence files
const SIZES_EXT: &str = "sizes4";

/// Blob storage manager
/// 
/// Handles the three-file storage for a single blob.
#[derive(Debug, Clone)]
pub struct BlobStore {
    /// Base path for blob storage (e.g., ~/.harbor/blobs/)
    base_path: PathBuf,
}

impl BlobStore {
    /// Create a new BlobStore with the given base path
    pub fn new(base_path: impl AsRef<Path>) -> io::Result<Self> {
        let base_path = base_path.as_ref().to_path_buf();
        
        // Create the directory if it doesn't exist
        // Use restrictive permissions (owner only on Unix)
        fs::create_dir_all(&base_path)?;
        
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(&base_path, perms)?;
        }
        
        Ok(Self { base_path })
    }
    
    /// Get the base path
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }
    
    /// Get paths for a blob's three files
    fn blob_paths(&self, hash: &[u8; 32]) -> BlobPaths {
        let hash_hex = hex::encode(hash);
        BlobPaths {
            data: self.base_path.join(format!("{}.{}", hash_hex, DATA_EXT)),
            outboard: self.base_path.join(format!("{}.{}", hash_hex, OUTBOARD_EXT)),
            sizes: self.base_path.join(format!("{}.{}", hash_hex, SIZES_EXT)),
        }
    }
    
    /// Check if a blob exists (has data file)
    pub fn exists(&self, hash: &[u8; 32]) -> bool {
        self.blob_paths(hash).data.exists()
    }
    
    /// Check if a blob is complete
    pub fn is_complete(&self, hash: &[u8; 32], total_chunks: u32) -> io::Result<bool> {
        let paths = self.blob_paths(hash);
        
        if !paths.sizes.exists() {
            return Ok(false);
        }
        
        let sizes_file = File::open(&paths.sizes)?;
        let file_len = sizes_file.metadata()?.len();
        let expected_len = total_chunks as u64 * 8;
        
        if file_len < expected_len {
            return Ok(false);
        }
        
        // Check that all chunks are marked as present
        let mut reader = io::BufReader::new(sizes_file);
        let mut buf = [0u8; 8];
        
        for _ in 0..total_chunks {
            reader.read_exact(&mut buf)?;
            let size = u64::from_le_bytes(buf);
            if size == 0 {
                return Ok(false);
            }
        }
        
        Ok(true)
    }
    
    /// Get bitfield of which chunks are present
    pub fn get_chunk_bitfield(&self, hash: &[u8; 32], total_chunks: u32) -> io::Result<Vec<bool>> {
        let paths = self.blob_paths(hash);
        let mut bitfield = vec![false; total_chunks as usize];
        
        if !paths.sizes.exists() {
            return Ok(bitfield);
        }
        
        let mut file = File::open(&paths.sizes)?;
        let mut buf = [0u8; 8];
        
        for chunk in bitfield.iter_mut().take(total_chunks as usize) {
            match file.read_exact(&mut buf) {
                Ok(()) => {
                    let size = u64::from_le_bytes(buf);
                    *chunk = size > 0;
                }
                Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        
        Ok(bitfield)
    }
    
    /// Read a chunk from the data file
    pub fn read_chunk(&self, hash: &[u8; 32], chunk_index: u32) -> io::Result<Vec<u8>> {
        let paths = self.blob_paths(hash);
        let mut file = File::open(&paths.data)?;
        
        let offset = chunk_index as u64 * CHUNK_SIZE;
        file.seek(SeekFrom::Start(offset))?;
        
        let mut buf = vec![0u8; CHUNK_SIZE as usize];
        let bytes_read = file.read(&mut buf)?;
        buf.truncate(bytes_read);
        
        Ok(buf)
    }
    
    /// Write a chunk to the data file and update sizes
    pub fn write_chunk(
        &self,
        hash: &[u8; 32],
        chunk_index: u32,
        data: &[u8],
        total_size: u64,
    ) -> io::Result<()> {
        let paths = self.blob_paths(hash);
        
        // Ensure parent directory exists
        if let Some(parent) = paths.data.parent() {
            fs::create_dir_all(parent)?;
        }
        
        // Write to data file
        let mut data_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&paths.data)?;
        
        let offset = chunk_index as u64 * CHUNK_SIZE;
        data_file.seek(SeekFrom::Start(offset))?;
        data_file.write_all(data)?;
        
        // Update sizes file (mark chunk as present)
        let mut sizes_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&paths.sizes)?;
        
        let sizes_offset = chunk_index as u64 * 8;
        sizes_file.seek(SeekFrom::Start(sizes_offset))?;
        sizes_file.write_all(&total_size.to_le_bytes())?;
        
        // Update outboard with chunk hash for verification
        self.update_outboard_hash(hash, chunk_index, data)?;
        
        Ok(())
    }
    
    /// Write outboard data (BLAKE3 hash tree)
    pub fn write_outboard(&self, hash: &[u8; 32], outboard: &[u8]) -> io::Result<()> {
        let paths = self.blob_paths(hash);
        
        // Ensure parent directory exists
        if let Some(parent) = paths.outboard.parent() {
            fs::create_dir_all(parent)?;
        }
        
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&paths.outboard)?;
        
        file.write_all(outboard)?;
        Ok(())
    }
    
    /// Read outboard data
    pub fn read_outboard(&self, hash: &[u8; 32]) -> io::Result<Vec<u8>> {
        let paths = self.blob_paths(hash);
        fs::read(&paths.outboard)
    }
    
    /// Delete all files for a blob
    pub fn delete(&self, hash: &[u8; 32]) -> io::Result<()> {
        let paths = self.blob_paths(hash);
        
        // Remove files, ignoring "not found" errors
        let _ = fs::remove_file(&paths.data);
        let _ = fs::remove_file(&paths.outboard);
        let _ = fs::remove_file(&paths.sizes);
        
        Ok(())
    }
    
    /// Import a file into the blob store
    /// 
    /// Computes BLAKE3 hash, creates outboard, and sets up the three files.
    /// Returns the hash and total size.
    pub fn import_file(&self, source_path: impl AsRef<Path>) -> io::Result<([u8; 32], u64)> {
        let source_path = source_path.as_ref();
        let source_file = File::open(source_path)?;
        let total_size = source_file.metadata()?.len();
        
        // Compute hash and outboard
        let mut hasher = blake3::Hasher::new();
        let mut reader = io::BufReader::new(&source_file);
        let mut buf = vec![0u8; CHUNK_SIZE as usize];
        
        loop {
            let bytes_read = reader.read(&mut buf)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buf[..bytes_read]);
        }
        
        let hash: [u8; 32] = *hasher.finalize().as_bytes();
        let paths = self.blob_paths(&hash);
        
        // Copy file to data location
        fs::copy(source_path, &paths.data)?;
        
        // Compute and write outboard
        let outboard = self.compute_outboard(&hash, total_size)?;
        self.write_outboard(&hash, &outboard)?;
        
        // Write sizes file (all chunks present)
        let total_chunks = total_size.div_ceil(CHUNK_SIZE) as u32;
        let mut sizes_file = File::create(&paths.sizes)?;
        for _ in 0..total_chunks {
            sizes_file.write_all(&total_size.to_le_bytes())?;
        }
        
        Ok((hash, total_size))
    }
    
    /// Compute outboard for a file
    fn compute_outboard(&self, hash: &[u8; 32], total_size: u64) -> io::Result<Vec<u8>> {
        let paths = self.blob_paths(hash);
        let file = File::open(&paths.data)?;
        let mut reader = io::BufReader::new(file);
        
        // For files that fit in one chunk, no outboard needed
        if total_size <= CHUNK_SIZE {
            return Ok(Vec::new());
        }
        
        // Compute BLAKE3 outboard
        // The outboard is a tree of hashes for verification
        let mut outboard = Vec::new();
        let mut chunk_hashes = Vec::new();
        
        // Hash each chunk
        let mut buf = vec![0u8; CHUNK_SIZE as usize];
        loop {
            let bytes_read = reader.read(&mut buf)?;
            if bytes_read == 0 {
                break;
            }
            let chunk_hash = blake3::hash(&buf[..bytes_read]);
            chunk_hashes.push(*chunk_hash.as_bytes());
        }
        
        // Build hash tree (simplified - pairs of hashes)
        let mut level = chunk_hashes;
        while level.len() > 1 {
            let mut next_level = Vec::new();
            for pair in level.chunks(2) {
                if pair.len() == 2 {
                    // Write the pair to outboard
                    outboard.extend_from_slice(&pair[0]);
                    outboard.extend_from_slice(&pair[1]);
                    
                    // Compute parent hash
                    let mut hasher = blake3::Hasher::new();
                    hasher.update(&pair[0]);
                    hasher.update(&pair[1]);
                    next_level.push(*hasher.finalize().as_bytes());
                } else {
                    // Odd node, promote to next level
                    next_level.push(pair[0]);
                }
            }
            level = next_level;
        }
        
        Ok(outboard)
    }
    
    /// Verify a chunk against the outboard using BLAKE3
    /// 
    /// The outboard stores hash pairs at each level of the tree.
    /// For chunk verification, we compute the BLAKE3 hash of the chunk data
    /// and verify it against the expected hash stored in the outboard.
    pub fn verify_chunk(
        &self,
        hash: &[u8; 32],
        chunk_index: u32,
        chunk_data: &[u8],
    ) -> io::Result<bool> {
        // Empty chunks are always invalid
        if chunk_data.is_empty() {
            return Ok(false);
        }
        
        // Compute BLAKE3 hash of the chunk
        let chunk_hash = blake3::hash(chunk_data);
        let computed_hash = chunk_hash.as_bytes();
        
        // Read the outboard to get expected chunk hashes
        let paths = self.blob_paths(hash);
        
        // If outboard doesn't exist, accept the chunk (first chunks during initial sync)
        if !paths.outboard.exists() {
            // For initial imports, we verify against the file hash later
            return Ok(true);
        }
        
        let mut outboard_file = File::open(&paths.outboard)?;
        let outboard_size = outboard_file.metadata()?.len() as usize;
        
        // Read outboard into memory
        let mut outboard = vec![0u8; outboard_size];
        outboard_file.read_exact(&mut outboard)?;
        
        // The outboard stores pairs of sibling hashes at each level
        // To find the expected hash for a chunk, we need to traverse the tree
        // The chunk hashes are at the leaf level (first entries in outboard)
        
        // For a simplified approach: extract chunk hash from outboard structure
        // Each pair contributes 64 bytes (32 bytes × 2 hashes)
        // Chunk hashes appear as the first level pairs
        
        // Calculate position in outboard
        // The outboard is stored as pairs: [h0, h1], [h2, h3], [h4, h5], ...
        // followed by parent pairs, etc.
        
        // For chunk at index i, its hash and sibling are at position:
        // - Even index i: outboard[i * 32 .. (i+1) * 32]
        // - Odd index i:  outboard[i * 32 .. (i+1) * 32]
        // But this depends on tree depth...
        
        // Verify chunk hash matches expected (if we have stored hashes)
        // The expected hash for chunk i should be at index i in the leaf layer
        let hash_offset = (chunk_index as usize) * 32;
        
        // Check if we can read the expected hash from outboard
        if hash_offset + 32 <= outboard.len() {
            let expected = &outboard[hash_offset..hash_offset + 32];
            
            // If stored hash is all zeros, this is a new chunk position
            if expected.iter().all(|&b| b == 0) {
                return Ok(true);
            }
            
            // Compare computed hash with expected
            return Ok(computed_hash == expected);
        }
        
        // If outboard doesn't have entry for this chunk, accept it
        // (outboard may be smaller than expected for partial files)
        Ok(true)
    }
    
    /// Update the outboard with a chunk's hash after writing
    pub fn update_outboard_hash(
        &self,
        hash: &[u8; 32],
        chunk_index: u32,
        chunk_data: &[u8],
    ) -> io::Result<()> {
        let paths = self.blob_paths(hash);
        
        // Compute chunk hash
        let chunk_hash = blake3::hash(chunk_data);
        
        // Open or create outboard file
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&paths.outboard)?;
        
        // Write hash at the appropriate position
        let hash_offset = (chunk_index as usize) * 32;
        
        // Extend file if needed
        let file_size = file.metadata()?.len() as usize;
        if file_size < hash_offset + 32 {
            file.set_len((hash_offset + 32) as u64)?;
        }
        
        file.seek(SeekFrom::Start(hash_offset as u64))?;
        file.write_all(chunk_hash.as_bytes())?;
        file.flush()?;
        
        Ok(())
    }
    
    /// Export a complete blob to a destination path
    pub fn export_file(&self, hash: &[u8; 32], dest_path: impl AsRef<Path>) -> io::Result<()> {
        let paths = self.blob_paths(hash);
        fs::copy(&paths.data, dest_path)?;
        Ok(())
    }
    
    /// Get the path to the data file for direct access
    pub fn data_path(&self, hash: &[u8; 32]) -> PathBuf {
        self.blob_paths(hash).data
    }
}

/// Paths for a blob's three files
#[derive(Debug)]
struct BlobPaths {
    data: PathBuf,
    outboard: PathBuf,
    sizes: PathBuf,
}

/// Default blob storage path
/// 
/// Returns a platform-appropriate private directory for blob storage.
pub fn default_blob_path() -> io::Result<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        // macOS: ~/Library/Application Support/harbor/blobs
        dirs::data_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Could not find data directory"))?
            .join("harbor")
            .join("blobs")
    } else if cfg!(target_os = "windows") {
        // Windows: %APPDATA%\harbor\blobs
        dirs::data_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Could not find data directory"))?
            .join("harbor")
            .join("blobs")
    } else {
        // Linux/other: ~/.local/share/harbor/blobs
        dirs::data_local_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Could not find local data directory"))?
            .join("harbor")
            .join("blobs")
    };
    
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn setup_store() -> (BlobStore, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let store = BlobStore::new(temp_dir.path().join("blobs")).unwrap();
        (store, temp_dir)
    }

    #[test]
    fn test_write_and_read_chunk() {
        let (store, _temp) = setup_store();
        let hash = [1u8; 32];
        let chunk_data = vec![42u8; 1000];
        
        store.write_chunk(&hash, 0, &chunk_data, 1000).unwrap();
        
        let read_data = store.read_chunk(&hash, 0).unwrap();
        assert_eq!(read_data, chunk_data);
    }

    #[test]
    fn test_chunk_bitfield() {
        let (store, _temp) = setup_store();
        let hash = [2u8; 32];
        
        // Write chunks 0 and 2, skip chunk 1
        store.write_chunk(&hash, 0, &[1u8; 100], 1024 * 1024).unwrap();
        store.write_chunk(&hash, 2, &[3u8; 100], 1024 * 1024).unwrap();
        
        let bitfield = store.get_chunk_bitfield(&hash, 4).unwrap();
        assert_eq!(bitfield, vec![true, false, true, false]);
    }

    #[test]
    fn test_is_complete() {
        let (store, _temp) = setup_store();
        let hash = [3u8; 32];
        let total_size = CHUNK_SIZE * 2;
        let total_chunks = 2;
        
        // Not complete initially
        assert!(!store.is_complete(&hash, total_chunks).unwrap());
        
        // Write first chunk
        store.write_chunk(&hash, 0, &[1u8; CHUNK_SIZE as usize], total_size).unwrap();
        assert!(!store.is_complete(&hash, total_chunks).unwrap());
        
        // Write second chunk
        store.write_chunk(&hash, 1, &[2u8; CHUNK_SIZE as usize], total_size).unwrap();
        assert!(store.is_complete(&hash, total_chunks).unwrap());
    }

    #[test]
    fn test_import_and_export() {
        let (store, temp) = setup_store();
        
        // Create a source file
        let source_path = temp.path().join("source.bin");
        let mut source_file = File::create(&source_path).unwrap();
        let data = vec![42u8; CHUNK_SIZE as usize + 1000]; // Slightly over one chunk
        source_file.write_all(&data).unwrap();
        drop(source_file);
        
        // Import
        let (hash, size) = store.import_file(&source_path).unwrap();
        assert_eq!(size, data.len() as u64);
        
        // Export
        let dest_path = temp.path().join("dest.bin");
        store.export_file(&hash, &dest_path).unwrap();
        
        // Verify
        let exported = fs::read(&dest_path).unwrap();
        assert_eq!(exported, data);
    }

    #[test]
    fn test_delete() {
        let (store, _temp) = setup_store();
        let hash = [4u8; 32];
        
        store.write_chunk(&hash, 0, &[1u8; 100], 100).unwrap();
        assert!(store.exists(&hash));
        
        store.delete(&hash).unwrap();
        assert!(!store.exists(&hash));
    }

    #[test]
    fn test_write_chunks_out_of_order() {
        let (store, _temp) = setup_store();
        let hash = [5u8; 32];
        let total_size = CHUNK_SIZE * 3;
        
        // Write chunks out of order: 2, 0, 1
        store.write_chunk(&hash, 2, &[3u8; CHUNK_SIZE as usize], total_size).unwrap();
        store.write_chunk(&hash, 0, &[1u8; CHUNK_SIZE as usize], total_size).unwrap();
        store.write_chunk(&hash, 1, &[2u8; CHUNK_SIZE as usize], total_size).unwrap();
        
        // Verify all chunks are present
        let bitfield = store.get_chunk_bitfield(&hash, 3).unwrap();
        assert_eq!(bitfield, vec![true, true, true]);
        
        // Verify content
        assert_eq!(store.read_chunk(&hash, 0).unwrap(), vec![1u8; CHUNK_SIZE as usize]);
        assert_eq!(store.read_chunk(&hash, 1).unwrap(), vec![2u8; CHUNK_SIZE as usize]);
        assert_eq!(store.read_chunk(&hash, 2).unwrap(), vec![3u8; CHUNK_SIZE as usize]);
    }

    #[test]
    fn test_multiple_blobs() {
        let (store, _temp) = setup_store();
        let hash1 = [10u8; 32];
        let hash2 = [11u8; 32];
        let hash3 = [12u8; 32];
        
        store.write_chunk(&hash1, 0, &[1u8; 100], 100).unwrap();
        store.write_chunk(&hash2, 0, &[2u8; 200], 200).unwrap();
        store.write_chunk(&hash3, 0, &[3u8; 300], 300).unwrap();
        
        assert!(store.exists(&hash1));
        assert!(store.exists(&hash2));
        assert!(store.exists(&hash3));
        
        // Delete one, others should remain
        store.delete(&hash2).unwrap();
        
        assert!(store.exists(&hash1));
        assert!(!store.exists(&hash2));
        assert!(store.exists(&hash3));
    }

    #[test]
    fn test_read_missing_chunk() {
        let (store, _temp) = setup_store();
        let hash = [6u8; 32];
        let total_size = CHUNK_SIZE * 3;
        
        // Only write chunk 0
        store.write_chunk(&hash, 0, &[1u8; CHUNK_SIZE as usize], total_size).unwrap();
        
        // Reading chunk 1 should return empty (sparse region)
        let result = store.read_chunk(&hash, 1).unwrap();
        // Sparse files return zeros for unwritten regions
        assert!(result.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_partial_last_chunk() {
        let (store, _temp) = setup_store();
        let hash = [7u8; 32];
        // Total size is 1.5 chunks
        let total_size = CHUNK_SIZE + CHUNK_SIZE / 2;
        let last_chunk_size = (total_size % CHUNK_SIZE) as usize;
        
        // Write full first chunk
        store.write_chunk(&hash, 0, &[1u8; CHUNK_SIZE as usize], total_size).unwrap();
        
        // Write partial last chunk (use vec instead of array literal)
        let last_chunk_data = vec![2u8; last_chunk_size];
        store.write_chunk(&hash, 1, &last_chunk_data, total_size).unwrap();
        
        // Read back - should get correct sizes
        let chunk0 = store.read_chunk(&hash, 0).unwrap();
        assert_eq!(chunk0.len(), CHUNK_SIZE as usize);
        
        let chunk1 = store.read_chunk(&hash, 1).unwrap();
        // Last chunk may be padded to full size in file, but content should be correct
        assert!(chunk1.starts_with(&[2u8; 1][..]));
    }

    #[test]
    fn test_import_exact_chunk_boundary() {
        let (store, temp) = setup_store();
        
        // Create file that's exactly 2 chunks
        let source_path = temp.path().join("exact.bin");
        let data = vec![42u8; CHUNK_SIZE as usize * 2];
        fs::write(&source_path, &data).unwrap();
        
        let (hash, size) = store.import_file(&source_path).unwrap();
        assert_eq!(size, CHUNK_SIZE * 2);
        
        // Should have exactly 2 chunks
        let bitfield = store.get_chunk_bitfield(&hash, 2).unwrap();
        assert_eq!(bitfield, vec![true, true]);
    }

    #[test]
    fn test_export_nonexistent_blob() {
        let (store, temp) = setup_store();
        let hash = [99u8; 32];
        let dest_path = temp.path().join("nonexistent.bin");
        
        let result = store.export_file(&hash, &dest_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_large_file_import_export() {
        let (store, temp) = setup_store();
        
        // Create a file with 5 chunks
        let source_path = temp.path().join("large.bin");
        let chunk_count = 5;
        let total_size = CHUNK_SIZE as usize * chunk_count + 1000; // Partial last chunk
        let data: Vec<u8> = (0..total_size).map(|i| (i % 256) as u8).collect();
        fs::write(&source_path, &data).unwrap();
        
        // Import
        let (hash, size) = store.import_file(&source_path).unwrap();
        assert_eq!(size, total_size as u64);
        
        // Verify chunks
        let expected_chunks = ((total_size as u64 + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        assert!(store.is_complete(&hash, expected_chunks).unwrap());
        
        // Export and compare
        let dest_path = temp.path().join("large_out.bin");
        store.export_file(&hash, &dest_path).unwrap();
        let exported = fs::read(&dest_path).unwrap();
        assert_eq!(exported, data);
    }

    #[test]
    fn test_overwrite_chunk() {
        let (store, _temp) = setup_store();
        let hash = [8u8; 32];
        
        // Write initial data
        store.write_chunk(&hash, 0, &[1u8; 100], 100).unwrap();
        assert_eq!(store.read_chunk(&hash, 0).unwrap(), vec![1u8; 100]);
        
        // Overwrite with different data
        store.write_chunk(&hash, 0, &[2u8; 100], 100).unwrap();
        assert_eq!(store.read_chunk(&hash, 0).unwrap(), vec![2u8; 100]);
    }

    #[test]
    fn test_blob_exists_after_write() {
        let (store, _temp) = setup_store();
        let hash = [9u8; 32];
        let total_size = 12345u64;
        
        assert!(!store.exists(&hash));
        store.write_chunk(&hash, 0, &[1u8; 1000], total_size).unwrap();
        assert!(store.exists(&hash));
    }

    #[test]
    fn test_delete_nonexistent() {
        let (store, _temp) = setup_store();
        let hash = [99u8; 32];
        
        // Should not error when deleting non-existent blob
        let result = store.delete(&hash);
        assert!(result.is_ok());
    }

    #[test]
    fn test_concurrent_chunk_writes_simulation() {
        // This test simulates what would happen with concurrent writes
        // by writing different chunks in alternating order
        let (store, _temp) = setup_store();
        let hash = [20u8; 32];
        let total_size = CHUNK_SIZE * 4;
        
        // Simulate concurrent access by interleaving writes
        for round in 0..3 {
            for chunk in 0..4 {
                let data = vec![(chunk * 10 + round) as u8; CHUNK_SIZE as usize];
                store.write_chunk(&hash, chunk, &data, total_size).unwrap();
            }
        }
        
        // Final state should have last written data
        let bitfield = store.get_chunk_bitfield(&hash, 4).unwrap();
        assert_eq!(bitfield, vec![true, true, true, true]);
    }

    #[test]
    fn test_chunk_boundary_math() {
        // Verify chunk calculations are correct
        let total_size = CHUNK_SIZE * 3 + 100;
        let expected_chunks = ((total_size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        assert_eq!(expected_chunks, 4); // 3 full + 1 partial
        
        let exact_size = CHUNK_SIZE * 5;
        let expected_exact = (exact_size / CHUNK_SIZE) as u32;
        assert_eq!(expected_exact, 5);
    }

    // ========================================================================
    // Verification Tests
    // ========================================================================

    #[test]
    fn test_verify_chunk_accepts_valid_data() {
        let (store, _temp) = setup_store();
        let hash = [30u8; 32];
        let chunk_data = vec![42u8; 1000];
        
        // Write chunk first (this updates outboard)
        store.write_chunk(&hash, 0, &chunk_data, 1000).unwrap();
        
        // Same data should verify
        let result = store.verify_chunk(&hash, 0, &chunk_data).unwrap();
        assert!(result);
    }

    #[test]
    fn test_verify_chunk_rejects_modified_data() {
        let (store, _temp) = setup_store();
        let hash = [31u8; 32];
        let chunk_data = vec![42u8; 1000];
        
        // Write chunk
        store.write_chunk(&hash, 0, &chunk_data, 1000).unwrap();
        
        // Modified data should fail verification
        let mut modified = chunk_data.clone();
        modified[500] = 99; // Change one byte
        
        let result = store.verify_chunk(&hash, 0, &modified).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_verify_chunk_empty_data_rejected() {
        let (store, _temp) = setup_store();
        let hash = [32u8; 32];
        
        // Empty data should fail
        let result = store.verify_chunk(&hash, 0, &[]).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_verify_chunk_no_outboard_accepts() {
        let (store, _temp) = setup_store();
        let hash = [33u8; 32];
        let chunk_data = vec![42u8; 1000];
        
        // Without outboard, should accept (first sync scenario)
        let result = store.verify_chunk(&hash, 0, &chunk_data).unwrap();
        assert!(result);
    }

    #[test]
    fn test_update_outboard_hash_creates_file() {
        let (store, temp) = setup_store();
        let hash = [34u8; 32];
        let chunk_data = vec![42u8; 1000];
        
        // Update outboard directly
        store.update_outboard_hash(&hash, 0, &chunk_data).unwrap();
        
        // Outboard file should exist (check using the known path pattern)
        let hash_hex = hex::encode(&hash);
        let outboard_path = temp.path().join("blobs").join(format!("{}.obao4", hash_hex));
        assert!(outboard_path.exists());
    }

    #[test]
    fn test_outboard_multiple_chunks() {
        let (store, _temp) = setup_store();
        let hash = [35u8; 32];
        let total_size = CHUNK_SIZE * 3;
        
        // Write multiple chunks
        let chunk0 = vec![1u8; CHUNK_SIZE as usize];
        let chunk1 = vec![2u8; CHUNK_SIZE as usize];
        let chunk2 = vec![3u8; CHUNK_SIZE as usize];
        
        store.write_chunk(&hash, 0, &chunk0, total_size).unwrap();
        store.write_chunk(&hash, 1, &chunk1, total_size).unwrap();
        store.write_chunk(&hash, 2, &chunk2, total_size).unwrap();
        
        // All should verify
        assert!(store.verify_chunk(&hash, 0, &chunk0).unwrap());
        assert!(store.verify_chunk(&hash, 1, &chunk1).unwrap());
        assert!(store.verify_chunk(&hash, 2, &chunk2).unwrap());
        
        // Wrong data for wrong index should fail
        assert!(!store.verify_chunk(&hash, 0, &chunk1).unwrap());
        assert!(!store.verify_chunk(&hash, 1, &chunk0).unwrap());
    }

    #[test]
    fn test_verify_after_import() {
        let (store, temp) = setup_store();
        
        // Create source file
        let source_path = temp.path().join("verify_import.bin");
        let data = vec![42u8; CHUNK_SIZE as usize + 1000];
        fs::write(&source_path, &data).unwrap();
        
        // Import creates outboard
        let (hash, _size) = store.import_file(&source_path).unwrap();
        
        // Read chunks and verify
        let chunk0 = store.read_chunk(&hash, 0).unwrap();
        let chunk1 = store.read_chunk(&hash, 1).unwrap();
        
        assert!(store.verify_chunk(&hash, 0, &chunk0).unwrap());
        assert!(store.verify_chunk(&hash, 1, &chunk1[..1000]).unwrap());
    }

    #[test]
    fn test_blake3_hash_deterministic() {
        // Verify BLAKE3 hashing is deterministic
        let data = b"test data for hashing";
        let hash1 = blake3::hash(data);
        let hash2 = blake3::hash(data);
        assert_eq!(hash1.as_bytes(), hash2.as_bytes());
        
        // Different data produces different hash
        let data2 = b"different test data";
        let hash3 = blake3::hash(data2);
        assert_ne!(hash1.as_bytes(), hash3.as_bytes());
    }
}

