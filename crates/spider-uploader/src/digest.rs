use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STD;

use crate::error::{Result, UploadError};

pub const BAIDU_CHUNK_SIZE: usize = 4 * 1024 * 1024;
pub const CLOUD189_SLICE_SIZE: usize = 10 * 1024 * 1024;
const STREAM_BUF_SIZE: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartSpec {
    pub index: usize,
    pub name: String,
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigests {
    pub file_size: u64,
    pub full_md5_hex: String,
    pub baidu_block_md5s: Vec<String>,
    pub cloud189_slice_md5: String,
    pub cloud189_parts: Vec<PartSpec>,
}

/// Compute all requested file digests (`full_md5_hex`, Baidu 4 MiB chunk MD5s,
/// and Cloud189 10 MiB part/slice MD5s) in ONE single streaming pass over `path`.
pub fn compute_file_digests(
    path: &Path,
    need_baidu: bool,
    need_cloud189: bool,
) -> Result<FileDigests> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(UploadError::InvalidConfig(format!(
            "upload target is not a regular file: {}",
            path.display()
        )));
    }
    compute_digests_from_reader(&mut file, need_baidu, need_cloud189)
}

pub(crate) fn compute_digests_from_reader(
    reader: &mut impl Read,
    need_baidu: bool,
    need_cloud189: bool,
) -> Result<FileDigests> {
    let mut buf = vec![0u8; STREAM_BUF_SIZE];
    let mut file_size: u64 = 0;

    let mut full_ctx = md5::Context::new();

    let mut baidu_ctx = md5::Context::new();
    let mut baidu_rem = BAIDU_CHUNK_SIZE;
    let mut baidu_has_bytes = false;
    let mut baidu_block_md5s = Vec::new();

    let mut cloud_ctx = md5::Context::new();
    let mut cloud_rem = CLOUD189_SLICE_SIZE;
    let mut cloud_offset: u64 = 0;
    let mut cloud_part_bytes: u64 = 0;
    let mut cloud_slice_hexes = Vec::new();
    let mut cloud189_parts = Vec::new();

    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        };

        let chunk = &buf[..n];
        file_size += n as u64;
        full_ctx.consume(chunk);

        if need_baidu {
            let mut pos = 0;
            while pos < n {
                let take = (n - pos).min(baidu_rem);
                baidu_ctx.consume(&chunk[pos..pos + take]);
                baidu_rem -= take;
                baidu_has_bytes = true;
                pos += take;

                if baidu_rem == 0 {
                    let digest = std::mem::replace(&mut baidu_ctx, md5::Context::new()).finalize();
                    baidu_block_md5s.push(format!("{digest:x}"));
                    baidu_rem = BAIDU_CHUNK_SIZE;
                    baidu_has_bytes = false;
                }
            }
        }

        if need_cloud189 {
            let mut pos = 0;
            while pos < n {
                let take = (n - pos).min(cloud_rem);
                cloud_ctx.consume(&chunk[pos..pos + take]);
                cloud_rem -= take;
                cloud_part_bytes += take as u64;
                pos += take;

                if cloud_rem == 0 {
                    let digest = std::mem::replace(&mut cloud_ctx, md5::Context::new()).finalize();
                    cloud_slice_hexes.push(hex::encode_upper(digest.0));
                    cloud189_parts.push(PartSpec {
                        index: cloud189_parts.len(),
                        name: BASE64_STD.encode(digest.0),
                        offset: cloud_offset,
                        len: cloud_part_bytes,
                    });
                    cloud_offset += cloud_part_bytes;
                    cloud_part_bytes = 0;
                    cloud_rem = CLOUD189_SLICE_SIZE;
                }
            }
        }
    }

    if need_baidu && baidu_has_bytes {
        let digest = baidu_ctx.finalize();
        baidu_block_md5s.push(format!("{digest:x}"));
    }

    if need_cloud189 && cloud_part_bytes > 0 {
        let digest = cloud_ctx.finalize();
        cloud_slice_hexes.push(hex::encode_upper(digest.0));
        cloud189_parts.push(PartSpec {
            index: cloud189_parts.len(),
            name: BASE64_STD.encode(digest.0),
            offset: cloud_offset,
            len: cloud_part_bytes,
        });
    }

    let full_md5_hex = format!("{:x}", full_ctx.finalize());
    let cloud189_slice_md5 = if !need_cloud189 {
        String::new()
    } else if cloud189_parts.len() > 1 {
        let joined = cloud_slice_hexes.join("\n");
        format!("{:x}", md5::compute(joined.as_bytes()))
    } else {
        full_md5_hex.clone()
    };

    Ok(FileDigests {
        file_size,
        full_md5_hex,
        baidu_block_md5s,
        cloud189_slice_md5,
        cloud189_parts,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    struct ShortChunkReader<'a> {
        data: &'a [u8],
        pos: usize,
        max_step: usize,
    }

    impl Read for ShortChunkReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.data.len() - self.pos;
            if remaining == 0 {
                return Ok(0);
            }
            let n = remaining.min(buf.len()).min(self.max_step);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn single_pass_matches_exact_baidu_and_cloud189_boundaries() {
        // 13 MiB exercises:
        // - Baidu: 3 full 4 MiB chunks + 1 MiB tail = 4 chunks
        // - Cloud189: 1 full 10 MiB slice + 3 MiB tail = 2 slices (multi-slice slice_md5 rule)
        let size = 13 * 1024 * 1024 + 777;
        let data: Vec<u8> = (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect();

        let temp_path = std::env::temp_dir().join(format!(
            "spider_uploader_digest_test_{}.bin",
            uuid::Uuid::new_v4()
        ));
        {
            let mut f = File::create(&temp_path).unwrap();
            f.write_all(&data).unwrap();
        }

        let digests = compute_file_digests(&temp_path, true, true).unwrap();
        let _ = std::fs::remove_file(&temp_path);

        assert_eq!(digests.file_size, size as u64);
        assert_eq!(digests.full_md5_hex, format!("{:x}", md5::compute(&data)));

        // Verify Baidu 4 MiB blocks
        let expected_baidu: Vec<String> = data
            .chunks(BAIDU_CHUNK_SIZE)
            .map(|c| format!("{:x}", md5::compute(c)))
            .collect();
        assert_eq!(digests.baidu_block_md5s, expected_baidu);

        // Verify Cloud189 10 MiB parts and multi-slice sliceMd5
        let mut expected_hexes = Vec::new();
        let mut expected_parts = Vec::new();
        let mut offset = 0u64;
        for (idx, chunk) in data.chunks(CLOUD189_SLICE_SIZE).enumerate() {
            let d = md5::compute(chunk);
            expected_hexes.push(hex::encode_upper(d.0));
            expected_parts.push(PartSpec {
                index: idx,
                name: BASE64_STD.encode(d.0),
                offset,
                len: chunk.len() as u64,
            });
            offset += chunk.len() as u64;
        }
        let expected_slice_md5 =
            format!("{:x}", md5::compute(expected_hexes.join("\n").as_bytes()));
        assert_eq!(digests.cloud189_parts, expected_parts);
        assert_eq!(digests.cloud189_slice_md5, expected_slice_md5);
    }

    #[test]
    fn digests_handle_short_reads_and_selective_flags() {
        let data: Vec<u8> = (0..(5 * 1024 * 1024 + 13))
            .map(|i| (i % 256) as u8)
            .collect();
        let mut reader = ShortChunkReader {
            data: &data,
            pos: 0,
            max_step: 4093,
        };
        let only_baidu = compute_digests_from_reader(&mut reader, true, false).unwrap();
        assert_eq!(only_baidu.baidu_block_md5s.len(), 2);
        assert!(only_baidu.cloud189_parts.is_empty());
        assert!(only_baidu.cloud189_slice_md5.is_empty());
        assert_eq!(
            only_baidu.full_md5_hex,
            format!("{:x}", md5::compute(&data))
        );

        let mut reader2 = ShortChunkReader {
            data: &data,
            pos: 0,
            max_step: 4093,
        };
        let only_cloud189 = compute_digests_from_reader(&mut reader2, false, true).unwrap();
        assert!(only_cloud189.baidu_block_md5s.is_empty());
        assert_eq!(only_cloud189.cloud189_parts.len(), 1);
        assert_eq!(only_cloud189.cloud189_slice_md5, only_cloud189.full_md5_hex);
    }

    #[test]
    fn empty_file_produces_expected_digests() {
        let mut empty: &[u8] = &[];
        let digests = compute_digests_from_reader(&mut empty, true, true).unwrap();
        assert_eq!(digests.file_size, 0);
        assert_eq!(digests.full_md5_hex, "d41d8cd98f00b204e9800998ecf8427e");
        assert!(digests.baidu_block_md5s.is_empty());
        assert!(digests.cloud189_parts.is_empty());
        assert_eq!(digests.cloud189_slice_md5, digests.full_md5_hex);
    }
}
