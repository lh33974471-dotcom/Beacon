use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 8] = b"WUIAR1\0\0";

#[derive(Clone, Default)]
pub struct Archive {
    entries: HashMap<String, Vec<u8>>,
}

fn normalize(name: &str) -> String {
    name.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

impl Archive {
    pub fn parse(bytes: &[u8]) -> Result<Archive, String> {
        let mut p = 0usize;
        let take = |p: &mut usize, n: usize| -> Result<&[u8], String> {
            let end = p.checked_add(n).ok_or("overflow")?;
            if end > bytes.len() {
                return Err("truncated archive".into());
            }
            let s = &bytes[*p..end];
            *p = end;
            Ok(s)
        };
        if take(&mut p, MAGIC.len())? != MAGIC {
            return Err("bad archive magic".into());
        }
        let count = u32::from_le_bytes(take(&mut p, 4)?.try_into().unwrap()) as usize;
        let mut entries = HashMap::with_capacity(count);
        for _ in 0..count {
            let name_len = u16::from_le_bytes(take(&mut p, 2)?.try_into().unwrap()) as usize;
            let name = String::from_utf8(take(&mut p, name_len)?.to_vec())
                .map_err(|_| "bad entry name".to_string())?;
            let data_len = u32::from_le_bytes(take(&mut p, 4)?.try_into().unwrap()) as usize;
            let data = take(&mut p, data_len)?.to_vec();
            entries.insert(normalize(&name), data);
        }
        Ok(Archive { entries })
    }

    pub fn write(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (name, data) in entries {
            let name = normalize(name);
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(&normalize(name)).map(|v| v.as_slice())
    }

    pub fn get_str(&self, name: &str) -> Option<String> {
        self.get(name)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn matches_patterns(name: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    patterns.iter().any(|p| {
        let p = p.trim().to_ascii_lowercase();
        match p.strip_prefix("*") {
            Some(suffix) => lower.ends_with(suffix),
            None => lower == p,
        }
    })
}

pub fn pack_dir(root: &Path, patterns: &[String]) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut children: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
        children.sort_by_key(|e| e.path());
        for entry in children {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                let name = normalize(&rel.to_string_lossy());
                if matches_patterns(&name, patterns) {
                    out.push((name, std::fs::read(&path)?));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
