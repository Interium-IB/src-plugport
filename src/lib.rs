use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char, c_void};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

const ID: &CStr = c"plugport";
const EXTENSION: &str = std::env::consts::DLL_EXTENSION;
const MAX_MESSAGE: usize = 4096;

type ReadString = unsafe extern "C" fn(*const c_char, *mut c_char, u32) -> i32;
type ReadSetting = unsafe extern "C" fn(*const c_char, *const c_char, *mut c_char, u32) -> i32;
type BeforeSend = unsafe extern "C" fn(i64, *const c_char, usize) -> *const c_char;
type AfterSend = unsafe extern "C" fn(i64, i64, usize);
type RegisterSend = unsafe extern "C" fn(Option<BeforeSend>, Option<AfterSend>, usize) -> i32;

#[repr(C)]
pub struct PluginApi {
    unused_core_slots: [*const c_void; 3],
    get_storage_dir: ReadString,
    unused_slots_4_to_17: [*const c_void; 14],
    register_message_send_hook: RegisterSend,
    unused_slots_19_to_52: [*const c_void; 34],
    get_setting: ReadSetting,
}

#[repr(C)]
pub struct Metadata {
    name: *const c_char,
    version: *const c_char,
    author: *const c_char,
    description: *const c_char,
}

unsafe impl Sync for Metadata {}

static METADATA: Metadata = Metadata {
    name: c"PlugPort".as_ptr(),
    version: c"1.0.0".as_ptr(),
    author: c"@liznulo".as_ptr(),
    description: c"Share installed MandaGram plugins and list them in the current chat.".as_ptr(),
};

static API: AtomicPtr<PluginApi> = AtomicPtr::new(ptr::null_mut());

thread_local! {
    static REPLACEMENT: std::cell::RefCell<CString> = std::cell::RefCell::new(CString::default());
}

#[unsafe(no_mangle)]
pub extern "C" fn mandagram_plugin_metadata() -> *const Metadata {
    &METADATA
}

#[unsafe(no_mangle)]
pub extern "C" fn mandagram_plugin_settings() -> *const c_char {
    c"[
        {\"key\":\"plf_cmd\",\"title\":\"Share plugin command\",\"type\":\"text\",\"default\":\".plf\",\"maxBytes\":\"64\"},
        {\"key\":\"chelp_cmd\",\"title\":\"List plugins command\",\"type\":\"text\",\"default\":\".chelp\",\"maxBytes\":\"64\"}
    ]".as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mandagram_plugin_init(api: *const PluginApi) -> bool {
    let Some(host) = (unsafe { api.as_ref() }) else {
        return false;
    };
    API.store(api.cast_mut(), Ordering::Release);
    unsafe { (host.register_message_send_hook)(Some(before_send), None, 0) == 0 }
}

unsafe extern "C" fn before_send(
    _chat_id: i64,
    text: *const c_char,
    _user_data: usize,
) -> *const c_char {
    let api = API.load(Ordering::Acquire);
    let Some(api) = (unsafe { api.as_ref() }) else {
        return ptr::null();
    };
    if text.is_null() {
        return ptr::null();
    }
    let Ok(text) = (unsafe { CStr::from_ptr(text) }).to_str() else {
        return ptr::null();
    };
    let mut words = text.split_whitespace();
    let Some(command) = words.next() else {
        return ptr::null();
    };

    let report = command_setting(api, c"chelp_cmd", ".chelp");
    let file = command_setting(api, c"plf_cmd", ".plf");
    let argument = words.next();
    let extra = words.next().is_some();
    let replacement = if command == report && argument.is_none() {
        Catalog::read(api).map(|catalog| catalog.report())
    } else if command == file && !extra {
        let Some(id) = argument else {
            return cancelled();
        };
        Catalog::read(api).and_then(|catalog| catalog.document(id))
    } else if command == report || command == file {
        return cancelled();
    } else {
        return ptr::null();
    };

    let Ok(result) = replacement else {
        return cancelled();
    };
    let Ok(encoded) = CString::new(result.to_string()) else {
        return cancelled();
    };
    REPLACEMENT.with(|storage| {
        storage.replace(encoded);
        storage.borrow().as_ptr()
    })
}

fn cancelled() -> *const c_char {
    c"".as_ptr()
}

fn command_setting(api: &PluginApi, key: &CStr, fallback: &str) -> String {
    let mut buffer = [0_u8; 66];
    let size = unsafe {
        (api.get_setting)(
            ID.as_ptr(),
            key.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    let value = read_buffer(&buffer, size).unwrap_or_default().trim();
    if value.starts_with('.') && value.len() > 1 && !value.chars().any(char::is_whitespace) {
        value.to_owned()
    } else {
        fallback.to_owned()
    }
}

fn read_buffer(buffer: &[u8], size: i32) -> io::Result<&str> {
    if size < 0 || size as usize >= buffer.len() - 1 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    std::str::from_utf8(&buffer[..size as usize])
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

struct InstalledPlugin {
    id: String,
    name: String,
    version: String,
    enabled: bool,
    path: PathBuf,
}

struct Catalog(Vec<InstalledPlugin>);

impl Catalog {
    fn read(api: &PluginApi) -> io::Result<Self> {
        let mut path = vec![0_u8; 128 * 1024];
        let size = unsafe {
            (api.get_storage_dir)(ID.as_ptr(), path.as_mut_ptr().cast(), path.len() as u32)
        };
        let storage = Path::new(read_buffer(&path, size)?);
        let plugin_data = storage.parent().ok_or(io::ErrorKind::InvalidData)?;
        let portable = plugin_data
            .ancestors()
            .nth(2)
            .ok_or(io::ErrorKind::InvalidData)?;
        let plugin_dir = portable.join("plugins");
        let root = portable.canonicalize()?;
        if plugin_dir.canonicalize()? != root.join("plugins") {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let state = plugin_data.join(".loader");
        let cache = read_optional(&state.join(".metadata_cache"))?;
        let metadata: HashMap<_, _> = cache
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(4, '\t');
                Some((fields.next()?, (fields.next()?, fields.next()?)))
            })
            .collect();
        let disabled = read_optional(&state.join(".disabled_plugins"))?;
        let disabled: HashSet<_> = disabled.lines().map(str::trim).collect();

        let mut entries = Vec::new();
        for entry in fs::read_dir(plugin_dir)? {
            let entry = entry?;
            let path = entry.path();
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default();
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if !extension.eq_ignore_ascii_case(EXTENSION)
                || id.starts_with('.')
                || !entry.file_type()?.is_file()
            {
                continue;
            }
            let (name, version) = metadata.get(id).copied().unwrap_or((id, "-"));
            entries.push(InstalledPlugin {
                id: id.to_owned(),
                name: single_line(name),
                version: single_line(version),
                enabled: !disabled.contains(id),
                path,
            });
        }
        entries.sort_unstable_by(|first, second| first.id.cmp(&second.id));
        Ok(Self(entries))
    }

    fn document(&self, id: &str) -> io::Result<Value> {
        let plugin = self
            .0
            .iter()
            .find(|entry| entry.id == id)
            .ok_or(io::ErrorKind::NotFound)?;
        let path = plugin.path.to_str().ok_or(io::ErrorKind::InvalidData)?;
        let caption: String = plugin.name.chars().take(240).collect();
        Ok(json!({
            "@type": "inputMessageDocument",
            "document": {
                "@type": "inputDocument",
                "document": { "@type": "inputFileLocal", "path": path },
                "disable_content_type_detection": true
            },
            "caption": { "@type": "formattedText", "text": caption, "entities": [] }
        }))
    }

    fn report(&self) -> Value {
        let enabled = self.0.iter().filter(|plugin| plugin.enabled).count();
        let footer = format!(
            "Plugins: {}. Enabled: {}. Disabled: {}.",
            self.0.len(),
            enabled,
            self.0.len() - enabled,
        );
        let mut text = String::new();
        let mut entities = Vec::new();
        let mut offset = 0;
        for plugin in &self.0 {
            let state = if plugin.enabled { "✅" } else { "❌" };
            let prefix = format!("[{}] {state} ", plugin.version);
            let id = single_line(&plugin.id);
            let row = format!("{prefix}{} ({id})\n", plugin.name);
            let row_len = row.encode_utf16().count();
            if offset + row_len + footer.encode_utf16().count() + 4 > MAX_MESSAGE {
                text.push_str("…\n");
                break;
            }
            let name_start = offset + prefix.encode_utf16().count();
            let name_len = plugin.name.encode_utf16().count();
            if name_len > 0 {
                entities.push(entity("textEntityTypeBold", name_start, name_len));
            }
            let id_start = name_start + name_len + 2;
            entities.push(entity(
                "textEntityTypeCode",
                id_start,
                id.encode_utf16().count(),
            ));
            text.push_str(&row);
            offset += row_len;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&footer);
        entities.insert(
            0,
            entity(
                "textEntityTypeExpandableBlockQuote",
                0,
                text.encode_utf16().count(),
            ),
        );
        json!({
            "@type": "inputMessageText",
            "text": { "@type": "formattedText", "text": text, "entities": entities },
            "clear_draft": true
        })
    }
}

fn entity(kind: &str, offset: usize, length: usize) -> Value {
    json!({"@type": "textEntity", "offset": offset, "length": length, "type": {"@type": kind}})
}

fn read_optional(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        result => result,
    }
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}
