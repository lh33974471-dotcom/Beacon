use super::local_file::LocalFile;
use crate::{platform::unix::local_file::construct_file_list, ClipboardFile, CliprdrError};
use hbb_common::{
    bytes::{BufMut, BytesMut},
    bytes_codec::MAX_PACKET_LENGTH,
    log,
};
use parking_lot::Mutex;
use std::{path::PathBuf, sync::Arc, time::SystemTime, usize};

lazy_static::lazy_static! {
    // local files are cached, this value should not be changed when copying files
    // Because `CliprdrFileContentsRequest` only contains the index of the file in the list.
    // We need to keep the file list in the same order as the remote side.
    // We may add a `FileId` field to `CliprdrFileContentsRequest` in the future.
    static ref CLIP_FILES: Arc<Mutex<ClipFiles>> = Default::default();
}

#[derive(Debug)]
enum FileContentsRequest {
    Size {
        stream_id: i32,
        file_idx: usize,
    },

    Range {
        stream_id: i32,
        file_idx: usize,
        offset: u64,
        length: u64,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct FileSig {
    size: u64,
    mtime: Option<SystemTime>,
    is_dir: bool,
}

fn fingerprint(files: &[String]) -> Vec<FileSig> {
    files
        .iter()
        .map(|s| match std::fs::metadata(s) {
            Ok(mt) => FileSig {
                size: mt.len(),
                mtime: mt.modified().ok(),
                is_dir: mt.is_dir(),
            },
            Err(_) => FileSig::default(),
        })
        .collect()
}

#[derive(Default)]
struct ClipFiles {
    files: Vec<String>,
    sigs: Vec<FileSig>,
    file_list: Vec<LocalFile>,
    first_file_index: usize,
    files_pdu: Vec<u8>,
}

impl ClipFiles {
    fn clear(&mut self) {
        self.files.clear();
        self.sigs.clear();
        self.file_list.clear();
        self.first_file_index = usize::MAX;
        self.files_pdu.clear();
    }

    fn sync_files(
        &mut self,
        clipboard_files: &[String],
        sigs: Vec<FileSig>,
    ) -> Result<(), CliprdrError> {
        let clipboard_paths = clipboard_files
            .iter()
            .map(|s| PathBuf::from(s))
            .collect::<Vec<_>>();
        self.file_list = construct_file_list(&clipboard_paths)?;
        self.first_file_index = self
            .file_list
            .iter()
            .position(|f| !f.path.is_dir())
            .unwrap_or(usize::MAX);
        self.files = clipboard_files.to_vec();
        self.sigs = sigs;
        Ok(())
    }

    fn build_file_list_pdu(&mut self) {
        let mut data = BytesMut::with_capacity(4 + 592 * self.file_list.len());
        data.put_u32_le(self.file_list.len() as u32);
        for file in self.file_list.iter() {
            data.put(file.as_bin().as_slice());
        }
        self.files_pdu = data.to_vec()
    }

    fn get_files_for_audit(&self, request: &FileContentsRequest) -> Option<ClipboardFile> {
        if let FileContentsRequest::Range {
            file_idx, offset, ..
        } = request
        {
            if *file_idx == self.first_file_index && *offset == 0 {
                let files: Vec<(String, u64)> = self
                    .file_list
                    .iter()
                    .filter_map(|f| {
                        if f.path.is_file() {
                            Some((f.path.to_string_lossy().to_string(), f.size))
                        } else {
                            None
                        }
                    })
                    .collect::<_>();
                if files.is_empty() {
                    return None;
                } else {
                    return Some(ClipboardFile::Files { files });
                }
            }
        }
        None
    }

    fn serve_file_contents(
        &mut self,
        conn_id: i32,
        request: FileContentsRequest,
    ) -> Result<ClipboardFile, CliprdrError> {
        let (file_idx, file_contents_resp) = match request {
            FileContentsRequest::Size {
                stream_id,
                file_idx,
            } => {
                log::debug!("file contents (size) requested from conn: {}", conn_id);
                let Some(file) = self.file_list.get(file_idx) else {
                    log::error!(
                        "invalid file index {} requested from conn: {}",
                        file_idx,
                        conn_id
                    );
                    return Err(CliprdrError::InvalidRequest {
                        description: format!(
                            "invalid file index {} requested from conn: {}",
                            file_idx, conn_id
                        ),
                    });
                };

                log::debug!(
                    "conn {} requested file-{}: {}",
                    conn_id,
                    file_idx,
                    file.name
                );

                let size = file.size;
                (
                    file_idx,
                    ClipboardFile::FileContentsResponse {
                        msg_flags: 0x1,
                        stream_id,
                        requested_data: size.to_le_bytes().to_vec(),
                    },
                )
            }
            FileContentsRequest::Range {
                stream_id,
                file_idx,
                offset,
                length,
            } => {
                log::debug!(
                    "file contents (range from {} length {}) request from conn: {}",
                    offset,
                    length,
                    conn_id
                );
                let Some(file) = self.file_list.get_mut(file_idx) else {
                    log::error!(
                        "invalid file index {} requested from conn: {}",
                        file_idx,
                        conn_id
                    );
                    return Err(CliprdrError::InvalidRequest {
                        description: format!(
                            "invalid file index {} requested from conn: {}",
                            file_idx, conn_id
                        ),
                    });
                };
                log::debug!(
                    "conn {} requested file-{}: {}",
                    conn_id,
                    file_idx,
                    file.name
                );

                if offset > file.size {
                    log::error!("invalid reading offset requested from conn: {}", conn_id);
                    return Err(CliprdrError::InvalidRequest {
                        description: format!(
                            "invalid reading offset requested from conn: {}",
                            conn_id
                        ),
                    });
                }
                let read_size = if length > file.size - offset {
                    file.size - offset
                } else {
                    length
                };
                if read_size > MAX_PACKET_LENGTH as u64 {
                    return Err(CliprdrError::InvalidRequest {
                        description: format!(
                            "file contents request of {} bytes exceeds the {} byte limit, conn: {}",
                            read_size, MAX_PACKET_LENGTH, conn_id
                        ),
                    });
                }

                let mut buf = vec![0u8; read_size as usize];

                file.read_exact_at(&mut buf, offset)?;

                (
                    file_idx,
                    ClipboardFile::FileContentsResponse {
                        msg_flags: 0x1,
                        stream_id,
                        requested_data: buf,
                    },
                )
            }
        };

        log::debug!("file contents sent to conn: {}", conn_id);
        // hot reload next file
        for next_file in self.file_list.iter_mut().skip(file_idx + 1) {
            if !next_file.is_dir {
                next_file.load_handle()?;
                break;
            }
        }
        Ok(file_contents_resp)
    }
}

#[inline]
pub fn clear_files() {
    CLIP_FILES.lock().clear();
}

pub fn read_file_contents(
    conn_id: i32,
    stream_id: i32,
    list_index: i32,
    dw_flags: i32,
    n_position_low: i32,
    n_position_high: i32,
    cb_requested: i32,
) -> Vec<Result<ClipboardFile, CliprdrError>> {
    let fcr = if dw_flags == 0x1 {
        FileContentsRequest::Size {
            stream_id,
            file_idx: list_index as usize,
        }
    } else if dw_flags == 0x2 {
        let offset = (n_position_high as u64) << 32 | n_position_low as u32 as u64;
        let length = cb_requested as u32 as u64;

        FileContentsRequest::Range {
            stream_id,
            file_idx: list_index as usize,
            offset,
            length,
        }
    } else {
        return vec![Err(CliprdrError::InvalidRequest {
            description: format!("got invalid FileContentsRequest, dw_flats: {dw_flags}"),
        })];
    };

    let mut clip_files = CLIP_FILES.lock();
    let mut res = vec![];
    if let Some(files_res) = clip_files.get_files_for_audit(&fcr) {
        res.push(Ok(files_res));
    }
    res.push(clip_files.serve_file_contents(conn_id, fcr));
    res
}

pub fn sync_files(files: &[String]) -> Result<(), CliprdrError> {
    let current = fingerprint(files);
    let mut files_lock = CLIP_FILES.lock();
    if files_lock.files == files
        && files_lock.sigs == current
        && !current.iter().any(|sig| sig.is_dir)
    {
        return Ok(());
    }
    files_lock.sync_files(files, current)?;
    Ok(files_lock.build_file_list_pdu())
}

pub fn get_file_list_pdu() -> Vec<u8> {
    CLIP_FILES.lock().files_pdu.clone()
}

#[cfg(test)]
mod sig_test {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut dir = std::env::temp_dir();
            dir.push(format!("hoptodesk_sig_test_{}_{}", tag, nanos));
            fs::create_dir_all(&dir).unwrap();
            TmpDir(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn path_str(p: &PathBuf) -> String {
        p.to_string_lossy().to_string()
    }

    #[test]
    fn fingerprint_missing_path_is_default() {
        let tmp = TmpDir::new("missing");
        let missing = path_str(&tmp.join("does_not_exist.bin"));
        let sigs = fingerprint(&[missing]);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0], FileSig::default());
        assert_eq!(sigs[0].mtime, None);
    }

    #[test]
    fn fingerprint_detects_inplace_edit() {
        let tmp = TmpDir::new("edit");
        let file = tmp.join("a.bin");
        fs::write(&file, b"small").unwrap();
        let p = path_str(&file);

        let before = fingerprint(&[p.clone()]);
        let again = fingerprint(&[p.clone()]);
        assert_eq!(before, again);
        assert_eq!(before[0].size, 5);
        assert!(!before[0].is_dir);

        fs::write(&file, b"much larger contents than before").unwrap();
        let after = fingerprint(&[p]);
        assert_ne!(before, after);
        assert!(after[0].size > before[0].size);
    }

    #[test]
    fn fingerprint_flags_directory() {
        let tmp = TmpDir::new("dir");
        let sub = tmp.join("subdir");
        fs::create_dir_all(&sub).unwrap();
        let sigs = fingerprint(&[path_str(&sub)]);
        assert_eq!(sigs.len(), 1);
        assert!(sigs[0].is_dir);
    }

    static CLIP_FILES_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn recopy_after_edit_refreshes_cached_size() {
        let _guard = CLIP_FILES_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TmpDir::new("recopy");
        let file = tmp.join("doc.bin");
        fs::write(&file, b"v1").unwrap();
        let files = vec![path_str(&file)];

        clear_files();

        sync_files(&files).unwrap();
        {
            let cache = CLIP_FILES.lock();
            let idx = cache.first_file_index;
            assert_eq!(cache.file_list[idx].size, 2);
        }

        fs::write(&file, b"v2 is bigger").unwrap();
        sync_files(&files).unwrap();
        {
            let cache = CLIP_FILES.lock();
            let idx = cache.first_file_index;
            assert_eq!(cache.file_list[idx].size, 12);
        }

        clear_files();
    }

    #[test]
    fn range_request_fields_are_unsigned() {
        use std::os::unix::fs::FileExt;

        let _guard = CLIP_FILES_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TmpDir::new("range");
        let file = tmp.join("sparse.bin");
        let past_2gib = (2u64 << 30) + 16;
        let handle = fs::File::create(&file).unwrap();
        handle.set_len(3 << 30).unwrap();
        handle.write_all_at(b"tail", past_2gib).unwrap();
        drop(handle);

        clear_files();
        sync_files(&[path_str(&file)]).unwrap();
        let idx = CLIP_FILES.lock().first_file_index as i32;

        let res = read_file_contents(1, 1, idx, 0x2, past_2gib as u32 as i32, 0, 4);
        assert!(matches!(
            res.as_slice(),
            [Ok(crate::ClipboardFile::FileContentsResponse { requested_data, .. })]
                if requested_data == b"tail"
        ));

        let res = read_file_contents(1, 1, idx, 0x2, 1, 0, -1);
        assert!(matches!(
            res.as_slice(),
            [Err(crate::CliprdrError::InvalidRequest { .. })]
        ));

        clear_files();
    }
}
