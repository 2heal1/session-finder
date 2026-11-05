use crate::model::{Message, Session};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize)]
pub struct TranscriptFile {
    pub path: PathBuf,
    pub format: &'static str,
    pub bytes: u64,
}

#[derive(Serialize)]
struct MessageLine<'a> {
    r#type: &'static str,
    #[serde(flatten)]
    message: &'a Message,
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "临时输出目录不是普通目录：{}",
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn identical(path: &Path, snapshot: &File) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    ensure!(metadata.is_file(), "临时输出路径不是普通文件");
    if metadata.len() != snapshot.metadata()?.len() {
        return Ok(false);
    }
    let mut existing = BufReader::new(File::open(path)?);
    let mut snapshot = BufReader::new(snapshot);
    let mut left = [0; 8192];
    let mut right = [0; 8192];
    loop {
        let n = existing.read(&mut left)?;
        if n == 0 {
            return Ok(snapshot.read(&mut right)? == 0);
        }
        if snapshot.read_exact(&mut right[..n]).is_err() || left[..n] != right[..n] {
            return Ok(false);
        }
    }
}

/// Materialize only the selected conversation. Stable contents preserve the
/// existing file; replacement is atomic, so readers never see partial JSONL.
pub fn materialize(
    source_identity: &str,
    session: &Session,
    messages: &[Message],
) -> Result<TranscriptFile> {
    ensure!(
        !session.id.is_empty()
            && session
                .id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'-'),
        "TRAE 会话 ID 不能用作文件名"
    );
    let mut directory = std::env::temp_dir()
        .canonicalize()
        .context("系统临时目录不可用")?;
    let namespace = format!("{:x}", Sha256::digest(source_identity.as_bytes()));
    for component in ["session-finder", "trae", &namespace] {
        directory.push(component);
        private_directory(&directory)?;
    }
    let path = directory.join(format!("{}.jsonl", session.id));
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer(
            &mut writer,
            &serde_json::json!({
                "type": "session_meta",
                "schemaVersion": 1,
                "session": session.public(),
            }),
        )?;
        writeln!(writer)?;
        for message in messages {
            serde_json::to_writer(
                &mut writer,
                &MessageLine {
                    r#type: "message",
                    message,
                },
            )?;
            writeln!(writer)?;
        }
        writer.flush()?;
    }
    let bytes = temporary.as_file().metadata()?.len();
    // Use an independent file cursor when comparing the just-written snapshot.
    if identical(&path, &File::open(temporary.path())?)? {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    } else {
        temporary.as_file().sync_all()?;
        temporary
            .persist(&path)
            .map_err(|e| e.error)
            .context("无法替换 TRAE 临时对话文件")?;
    }
    Ok(TranscriptFile {
        path,
        format: "jsonl",
        bytes,
    })
}
