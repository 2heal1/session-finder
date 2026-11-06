//! Minimal ContextFS read-only TTHeader/Thrift client. No ctx-cli subprocess.
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

pub const INDEX: &str = "context://history/conversations_metadata.jsonl";
const READER: &str = "Contents/Resources/app/modules/ai-agent/bin/ctx-cli";
const MAX_FRAME: usize = 32 * 1024 * 1024;
const METHOD: &str = "ExecuteCommand";
static IDE_BUNDLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<key>\s*CFBundleIdentifier\s*</key>\s*<string>\s*(?:cn\.trae\.app|com\.trae\.app)\s*</string>").unwrap()
});

fn is_ide(app: &Path) -> bool {
    // Electron's shipped Info.plist is XML. Unknown/binary manifests are not
    // guessed; an explicit reader path remains available for those installs.
    app.join(READER).is_file()
        && std::fs::read_to_string(app.join("Contents/Info.plist"))
            .is_ok_and(|manifest| IDE_BUNDLE.is_match(&manifest))
}

/// The reader location identifies an installation/cache namespace, not a process to launch.
pub fn discover(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("SESSION_FINDER_TRAE_CTX").map(PathBuf::from))
    {
        return path
            .canonicalize()
            .context("Specified TRAE ctx-cli does not exist");
    }
    if let Some(root) = std::env::var_os("ICUBE_APP_ROOT") {
        let path = PathBuf::from(root).join("modules/ai-agent/bin/ctx-cli");
        if path.ancestors().nth(7).is_some_and(is_ide) {
            return Ok(path.canonicalize()?);
        }
    }
    let mut candidates = Vec::new();
    for directory in [
        PathBuf::from("/Applications"),
        dirs::home_dir().unwrap_or_default().join("Applications"),
    ] {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                if is_ide(&entry.path()) {
                    candidates.push(entry.path().join(READER).canonicalize()?);
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    ensure!(
        candidates.len() == 1,
        "Found {} TRAE IDE installations; select one with --trae-ctx /path/to/ctx-cli",
        candidates.len()
    );
    Ok(candidates.remove(0))
}

pub trait Reader {
    fn read(&self, path: &str) -> Result<String>;
}

pub struct Client {
    socket: PathBuf,
    token: String,
}

impl Client {
    pub fn from_env(reader: &Path) -> Result<Self> {
        let token = std::env::var("CFS_TOKEN").ok().filter(|s| !s.is_empty());
        let socket = std::env::var_os("CFS_IPC_NAME").filter(|s| !s.is_empty());
        ensure!(
            token.is_some()
                && socket.is_some()
                && std::env::var("CFS_TRANSPORT").as_deref() == Ok("ipc"),
            "Missing ContextFS IPC credentials. Run search --provider trae --refresh or sync --provider trae in the TRAE Agent environment; ordinary terminals can then search local snapshots"
        );
        if let Some(root) = std::env::var_os("ICUBE_APP_ROOT") {
            let runtime = PathBuf::from(root)
                .join("modules/ai-agent/bin/ctx-cli")
                .canonicalize()
                .context("Cannot identify the current TRAE application")?;
            ensure!(
                runtime == reader.canonicalize()?,
                "Specified TRAE installation does not match the current Agent application"
            );
        }
        Ok(Self {
            socket: PathBuf::from(socket.unwrap()),
            token: token.unwrap(),
        })
    }
}

fn string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as i32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}
fn field(out: &mut Vec<u8>, kind: u8, id: i16) {
    out.push(kind);
    out.extend_from_slice(&id.to_be_bytes());
}

fn request(path: &str, token: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0x80010001u32.to_be_bytes());
    string(&mut body, METHOD);
    body.extend_from_slice(&0i32.to_be_bytes());
    field(&mut body, 12, 1);
    field(&mut body, 11, 1);
    string(&mut body, "ctx-cli");
    field(&mut body, 11, 2);
    string(&mut body, "icube.common.commands.tooling.readFile");
    field(&mut body, 15, 3);
    body.push(11);
    body.extend_from_slice(&1i32.to_be_bytes());
    string(
        &mut body,
        &serde_json::json!({"filePath": path}).to_string(),
    );
    field(&mut body, 10, 4);
    body.extend_from_slice(&30000i64.to_be_bytes());
    field(&mut body, 13, 10);
    body.extend_from_slice(&[11, 11]);
    body.extend_from_slice(&1i32.to_be_bytes());
    string(&mut body, "cfs_process_token");
    string(&mut body, token);
    body.extend_from_slice(&[0, 0]);
    frame(&body)
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 22);
    out.extend_from_slice(&((body.len() + 18) as u32).to_be_bytes());
    out.extend_from_slice(&[0x10, 0, 0, 0]); // TTHeader magic + flags
    out.extend_from_slice(&0u32.to_be_bytes()); // sequence
    out.extend_from_slice(&1u16.to_be_bytes()); // header size in four-byte words
    out.extend_from_slice(&[0, 0, 0, 0]); // binary protocol, no transforms, padding
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

impl Reader for Client {
    fn read(&self, path: &str) -> Result<String> {
        ensure!(
            path.starts_with("context://history/"),
            "Only conversation history reads are supported"
        );
        let mut socket = UnixStream::connect(&self.socket).context(
            "Cannot connect to TRAE ContextFS; reopen the TRAE Agent environment and retry, then restart the IDE if needed. Existing local snapshots can still be searched without --refresh",
        )?;
        socket.set_read_timeout(Some(Duration::from_secs(35)))?;
        socket.set_write_timeout(Some(Duration::from_secs(35)))?;
        socket
            .write_all(&request(path, &self.token))
            .context("Cannot send ContextFS read request")?;
        let mut length = [0; 4];
        socket
            .read_exact(&mut length)
            .context("Cannot read ContextFS response header")?;
        let length = u32::from_be_bytes(length) as usize;
        ensure!(
            (14..=MAX_FRAME).contains(&length),
            "Invalid ContextFS response length"
        );
        let mut bytes = vec![0; length];
        socket
            .read_exact(&mut bytes)
            .context("Cannot read ContextFS response")?;
        response(&bytes)
    }
}

#[derive(Debug)]
enum Value {
    Number(i64),
    Text(String),
    Fields(BTreeMap<i16, Value>),
    Other,
}
impl Value {
    fn get(&self, id: i16) -> Result<&Value> {
        match self {
            Self::Fields(fields) => fields.get(&id).context("ContextFS response field missing"),
            _ => bail!("Invalid ContextFS response structure"),
        }
    }
    fn text(&self) -> Result<&str> {
        match self {
            Self::Text(s) => Ok(s),
            _ => bail!("Invalid ContextFS response text"),
        }
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
}
impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.bytes.len(), "Truncated ContextFS response");
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.i32()?;
        ensure!(
            n >= 0 && n as usize <= self.bytes.len(),
            "Invalid ContextFS field length"
        );
        Ok(n as usize)
    }
    fn text(&mut self) -> Result<String> {
        let n = self.count()?;
        // Do not attach UTF-8/JSON errors with payload text: it may contain private content.
        String::from_utf8(self.take(n)?.to_vec())
            .map_err(|_| anyhow::anyhow!("Invalid ContextFS text encoding"))
    }
    fn value(&mut self, kind: u8, depth: usize) -> Result<Value> {
        ensure!(depth < 32, "ContextFS response nesting is too deep");
        Ok(match kind {
            2 | 3 => Value::Number(self.byte()? as i8 as i64),
            6 => Value::Number(self.i16()? as i64),
            8 => Value::Number(self.i32()? as i64),
            10 => Value::Number(i64::from_be_bytes(self.take(8)?.try_into()?)),
            4 => {
                self.take(8)?;
                Value::Other
            }
            11 => Value::Text(self.text()?),
            12 => {
                let mut fields = BTreeMap::new();
                loop {
                    let kind = self.byte()?;
                    if kind == 0 {
                        break;
                    }
                    let id = self.i16()?;
                    let value = self.value(kind, depth + 1)?;
                    ensure!(
                        fields.insert(id, value).is_none(),
                        "Duplicate ContextFS response field"
                    );
                }
                Value::Fields(fields)
            }
            13 => {
                let key = self.byte()?;
                let val = self.byte()?;
                let count = self.count()?;
                for _ in 0..count {
                    self.value(key, depth + 1)?;
                    self.value(val, depth + 1)?;
                }
                Value::Other
            }
            14 | 15 => {
                let kind = self.byte()?;
                let count = self.count()?;
                for _ in 0..count {
                    self.value(kind, depth + 1)?;
                }
                Value::Other
            }
            _ => bail!("Unsupported ContextFS response field type"),
        })
    }
}

fn response(bytes: &[u8]) -> Result<String> {
    ensure!(
        bytes.len() <= MAX_FRAME,
        "ContextFS response exceeds size limit"
    );
    let mut d = Decoder { bytes };
    ensure!(
        d.i16()? == 0x1000 && d.i16()? == 0 && d.i32()? == 0,
        "Invalid ContextFS TTHeader"
    );
    let words = d.i16()?;
    ensure!(words > 0, "Invalid ContextFS TTHeader length");
    let header = d.take(words as usize * 4)?;
    ensure!(
        header.starts_with(&[0, 0]),
        "Unsupported ContextFS protocol or compression"
    );
    let mut version = d.i32()? as u32;
    if version & 0xffff0000 != 0x80010000 {
        ensure!(
            version as usize == d.bytes.len(),
            "ContextFS Thrift frame length mismatch"
        );
        version = d.i32()? as u32;
    }
    ensure!(
        version == 0x80010002,
        "ContextFS returned an exception or unsupported message"
    );
    ensure!(
        d.text()? == METHOD && d.i32()? == 0,
        "ContextFS response method or sequence mismatch"
    );
    let result = d.value(12, 0)?;
    ensure!(
        d.bytes.is_empty(),
        "Unexpected trailing ContextFS response data"
    );
    let result = result.get(0)?;
    let status = result.get(1)?;
    ensure!(
        status.get(1)?.text()? == "ok" && matches!(status.get(2)?, Value::Number(0)),
        "ContextFS service rejected the read request"
    );
    let payload: serde_json::Value = serde_json::from_str(result.get(2)?.get(1)?.text()?)
        .map_err(|_| anyhow::anyhow!("ContextFS returned invalid JSON"))?;
    ensure!(
        payload["code"].as_i64() == Some(0),
        "ContextFS file read failed"
    );
    payload["data"]
        .as_str()
        .map(str::to_owned)
        .context("ContextFS returned no file content")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_accepts_renamed_ide_and_excludes_solo() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("renamed.app");
        let reader = app.join(READER);
        std::fs::create_dir_all(reader.parent().unwrap()).unwrap();
        std::fs::write(reader, "").unwrap();
        for (bundle, expected) in [
            ("cn.trae.app", true),
            ("com.trae.app", true),
            ("com.trae.solo.app", false),
            ("com.example.app", false),
        ] {
            std::fs::write(app.join("Contents/Info.plist"),
                format!("<plist><dict><key>CFBundleIdentifier</key>\n<string>{bundle}</string></dict></plist>")).unwrap();
            assert_eq!(is_ide(&app), expected);
        }
    }

    fn reply(data: &str, status: i32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0x80010002u32.to_be_bytes());
        string(&mut body, METHOD);
        body.extend_from_slice(&0i32.to_be_bytes());
        field(&mut body, 12, 0);
        field(&mut body, 12, 1);
        field(&mut body, 11, 1);
        string(&mut body, "ok");
        field(&mut body, 8, 2);
        body.extend_from_slice(&status.to_be_bytes());
        body.push(0);
        field(&mut body, 12, 2);
        field(&mut body, 11, 1);
        string(
            &mut body,
            &serde_json::json!({"code":0,"data":data}).to_string(),
        );
        body.extend_from_slice(&[0, 0, 0]);
        frame(&body)
    }

    #[test]
    fn actual_unix_round_trip_and_request_contract() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ipc");
        let listener = UnixListener::bind(&socket).unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut n = [0; 4];
            stream.read_exact(&mut n).unwrap();
            let mut bytes = vec![0; u32::from_be_bytes(n) as usize];
            stream.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes[..14], &[16, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
            let mut d = Decoder {
                bytes: &bytes[18..],
            };
            assert_eq!(d.i32().unwrap() as u32, 0x80010001);
            assert_eq!(d.text().unwrap(), METHOD);
            assert_eq!(d.i32().unwrap(), 0);
            let args = d.value(12, 0).unwrap();
            let req = args.get(1).unwrap();
            assert_eq!(req.get(1).unwrap().text().unwrap(), "ctx-cli");
            assert_eq!(
                req.get(2).unwrap().text().unwrap(),
                "icube.common.commands.tooling.readFile"
            );
            assert!(matches!(req.get(4).unwrap(), Value::Number(30000)));
            let raw = String::from_utf8_lossy(&bytes);
            assert!(raw.contains(INDEX));
            assert!(raw.contains("test-token"));
            assert!(raw.contains("cfs_process_token"));
            // Deliberately fragment the response to exercise read_exact.
            for chunk in reply("hello 中文\n", 0).chunks(3) {
                stream.write_all(chunk).unwrap();
            }
        });
        let client = Client {
            socket,
            token: "test-token".into(),
        };
        assert_eq!(client.read(INDEX).unwrap(), "hello 中文\n");
        thread.join().unwrap();
    }

    #[test]
    fn rejects_truncation_status_exceptions_and_oversized_fields() {
        let good = reply("private data", 0);
        assert_eq!(response(&good[4..]).unwrap(), "private data");
        for n in 4..good.len() {
            assert!(response(&good[4..n]).is_err(), "accepted prefix {n}");
        }
        assert!(response(&reply("secret", 1)[4..]).is_err());
        let mut exception = good.clone();
        exception[25] = 3;
        assert!(response(&exception[4..]).is_err());
        let mut huge = good.clone();
        huge[26..30].copy_from_slice(&i32::MAX.to_be_bytes());
        assert!(response(&huge[4..]).is_err());
        let mut d = Decoder {
            bytes: &[0, 0, 0, 1, 0],
        };
        assert!(d.value(15, 0).is_err());
    }
}
