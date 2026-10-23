//! AI activity monitor: which processes on this machine run AI, and what they use (GPU, CPU, memory,
//! Neural Engine). Observe only; needs no special permission.
//!
//! A process counts as AI when it holds model weights (a model file named on its command line, or open or
//! memory-mapped: GGUF, safetensors, Core ML, ONNX, ...) or Neural Engine memory. Counters are cumulative,
//! so a report is the difference between two samples. Cloud AI apps run elsewhere and do not show here.
use anyhow::Result;
use platform::ProcStat;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Smaller files with model magic bytes are fixtures or headers, not weights.
const MIN_MODEL_BYTES: u64 = 1 << 20;
/// A process this busy gets its open files searched for models even without GPU or Neural Engine use.
const BUSY_CPU_PCT: f64 = 20.0;

pub struct Sample {
    at: f64,
    procs: HashMap<i32, ProcStat>,
    gpu: HashMap<i32, (u64, String)>,
}

pub fn sample() -> Result<Sample> {
    Ok(Sample { gpu: platform::gpu_ns()?, procs: platform::proc_stats()?, at: platform::now() })
}

pub struct Row {
    pub pid: i32,
    pub name: String,
    /// Share of the window the GPU spent on this process, percent.
    pub gpu: f64,
    /// Percent of one core.
    pub cpu: f64,
    pub cpu_watts: f64,
    pub memory: u64,
    pub neural: u64,
    pub model: Option<String>,
}

pub struct Report {
    pub window_s: f64,
    pub rows: Vec<Row>,
    /// GPU busy share summed over all processes, AI or not.
    pub gpu_all: f64,
}

/// Processes running AI between `a` and `b`, busiest GPU first; `all` adds every other process that used the GPU.
pub fn report(a: &Sample, b: &Sample, all: bool) -> Result<Report> {
    let w = (b.at - a.at).max(1e-3);
    let pct = |d: u64| d as f64 / (w * 1e9) * 100.0;
    let gpu = |pid: i32| pct(b.gpu.get(&pid).map_or(0, |g| g.0).saturating_sub(a.gpu.get(&pid).map_or(0, |g| g.0)));
    let delta = |pid: i32, f: fn(&ProcStat) -> u64| b.procs.get(&pid).map_or(0, f).saturating_sub(a.procs.get(&pid).map_or(0, f));
    let cpu = |pid: i32| pct(delta(pid, |p| p.cpu_ns));
    let busy: Vec<i32> = b.procs.iter()
        .filter(|&(&pid, p)| b.gpu.contains_key(&pid) || p.neural > 0 || cpu(pid) >= BUSY_CPU_PCT)
        .map(|(&pid, _)| pid).collect();
    let models = models(&busy)?;
    let pids: BTreeSet<i32> = b.procs.keys().chain(b.gpu.keys()).copied().collect();
    let mut rows: Vec<Row> = pids.into_iter().filter_map(|pid| {
        let p = b.procs.get(&pid);
        let model = models.get(&pid).cloned();
        let ai = model.is_some() || p.is_some_and(|p| p.neural > 0);
        if !ai && !(all && gpu(pid) > 0.0) {
            return None;
        }
        Some(Row {
            pid,
            name: p.map(|p| p.name.clone()).or_else(|| b.gpu.get(&pid).map(|g| g.1.clone())).unwrap_or_default(),
            gpu: gpu(pid),
            cpu: cpu(pid),
            cpu_watts: delta(pid, |p| p.cpu_energy_nj) as f64 / (w * 1e9),
            memory: p.map_or(0, |p| p.footprint),
            neural: p.map_or(0, |p| p.neural),
            model,
        })
    }).collect();
    rows.sort_by(|x, y| y.gpu.total_cmp(&x.gpu).then(y.memory.cmp(&x.memory)));
    let gpu_all = b.gpu.keys().map(|&pid| gpu(pid)).sum();
    Ok(Report { window_s: w, rows, gpu_all })
}

/// Label of the largest model each process holds. Command lines are checked for every process, open files
/// only for `busy` ones (lsof is the slow part).
fn models(busy: &[i32]) -> Result<HashMap<i32, String>> {
    let mut files = platform::open_files(busy)?;
    // ponytail: whitespace-split command lines, so a model path with spaces is found only when open or mapped
    for (pid, cmd) in platform::command_lines()? {
        let paths = cmd.split_whitespace().skip(1).map(|t| t.rsplit_once('=').map_or(t, |(_, v)| v)).filter(|t| t.starts_with('/'));
        files.extend(paths.map(|t| (pid, PathBuf::from(t))));
    }
    let (mut seen, mut best) = (HashSet::new(), HashMap::<i32, (u64, String)>::new());
    for (pid, f) in files {
        if !seen.insert((pid, f.clone())) {
            continue;
        }
        if let Some((bytes, label)) = model_file(&f) {
            let e = best.entry(pid).or_default();
            if bytes >= e.0 {
                *e = (bytes, label);
            }
        }
    }
    Ok(best.into_iter().map(|(pid, (_, label))| (pid, label)).collect())
}

/// (bytes, label) when `path` holds model weights: a model format by name, or GGUF / safetensors by content.
fn model_file(path: &Path) -> Option<(u64, String)> {
    let bytes = platform::file_len(path)?;
    let s = path.to_string_lossy();
    // compiled Core ML models are directories; the process maps the weight files inside
    if let Some(m) = s.split('/').find(|c| c.ends_with(".mlmodelc") || c.ends_with(".mlpackage")) {
        return Some((bytes, format!("{} (Core ML)", m.rsplit_once('.').map_or(m, |x| x.0))));
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let format = match ext.as_str() {
        "gguf" | "ggml" => "GGUF",
        "safetensors" => "safetensors",
        "onnx" => "ONNX",
        "tflite" => "TFLite",
        "mlmodel" => "Core ML",
        "pt" | "pth" | "ckpt" => "PyTorch",
        _ if bytes < MIN_MODEL_BYTES => return None,
        _ => match platform::read_head(path, 9)? {
            h if h.starts_with(b"GGUF") => "GGUF",
            h if is_safetensors(&h) => "safetensors",
            _ => return None,
        },
    };
    let name = (format == "GGUF").then(|| platform::read_head(path, 1 << 16)).flatten().and_then(|h| gguf_name(&h))
        .or_else(|| hf_repo(&s))
        .unwrap_or_else(|| path.file_name().map_or(s.to_string(), |n| n.to_string_lossy().into_owned()));
    Some((bytes, format!("{name} ({format} {})", size(bytes))))
}

/// safetensors: a little-endian u64 header length, then the JSON header.
fn is_safetensors(h: &[u8]) -> bool {
    h.len() >= 9 && h[8] == b'{' && (2..100 << 20).contains(&u64::from_le_bytes(h[..8].try_into().unwrap()))
}

/// "org/name" of a Hugging Face cache path (`.../models--org--name/blobs/<hash>`).
fn hf_repo(path: &str) -> Option<String> {
    path.split('/').find_map(|c| c.strip_prefix("models--")).map(|r| r.replace("--", "/"))
}

fn take<'a>(h: &'a [u8], at: &mut usize, n: usize) -> Option<&'a [u8]> {
    let s = h.get(*at..at.checked_add(n)?)?;
    *at += n;
    Some(s)
}

fn int(h: &[u8], at: &mut usize, n: usize) -> Option<u64> {
    Some(take(h, at, n)?.iter().rev().fold(0, |v, &b| v << 8 | b as u64))
}

/// Steps over one GGUF metadata value of type `ty`.
fn skip(h: &[u8], at: &mut usize, ty: u64) -> Option<()> {
    let fixed = |ty| match ty {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    };
    let n = match ty {
        8 => int(h, at, 8)? as usize,
        9 => {
            let (et, count) = (int(h, at, 4)?, int(h, at, 8)? as usize);
            match fixed(et) {
                Some(sz) => count.checked_mul(sz)?,
                None => return (0..count).try_for_each(|_| skip(h, at, et)),
            }
        }
        t => fixed(t)?,
    };
    take(h, at, n).map(|_| ())
}

/// `general.name` from a GGUF header (v2+). It comes before the tokenizer arrays, so a header prefix is enough.
fn gguf_name(h: &[u8]) -> Option<String> {
    let mut at = 8; // magic, version
    let (_tensors, kvs) = (int(h, &mut at, 8)?, int(h, &mut at, 8)?);
    for _ in 0..kvs.min(64) {
        let n = int(h, &mut at, 8)? as usize;
        let key = take(h, &mut at, n)?;
        let ty = int(h, &mut at, 4)?;
        if key == b"general.name" && ty == 8 {
            let n = int(h, &mut at, 8)? as usize;
            return Some(String::from_utf8_lossy(take(h, &mut at, n)?).into_owned());
        }
        skip(h, &mut at, ty)?;
    }
    None
}

pub fn size(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{} MB", b >> 20),
        b => format!("{} KB", b >> 10),
    }
}

pub fn render(r: &Report) -> String {
    if r.rows.is_empty() {
        return format!("no AI processes (window {:.1}s, GPU busy {:.1}%)", r.window_s, r.gpu_all);
    }
    let mut out = format!("{:>6}  {:<18} {:>6} {:>6} {:>6} {:>8} {:>7}  MODEL\n", "PID", "PROCESS", "GPU", "CPU", "CPU W", "MEMORY", "NEURAL");
    for x in &r.rows {
        let neural = if x.neural > 0 { size(x.neural) } else { "-".into() };
        out += &format!("{:>6}  {:<18} {:>5.1}% {:>5.1}% {:>6.2} {:>8} {:>7}  {}\n", x.pid, x.name.chars().take(18).collect::<String>(),
                        x.gpu, x.cpu, x.cpu_watts, size(x.memory), neural, x.model.as_deref().unwrap_or("-"));
    }
    let ai: Vec<&Row> = r.rows.iter().filter(|x| x.model.is_some() || x.neural > 0).collect();
    out + &format!("AI: GPU {:.1}%, CPU {:.1}%, {:.2} W CPU, memory {}. GPU busy, all processes: {:.1}%. Window {:.1}s.",
                   ai.iter().map(|x| x.gpu).sum::<f64>(), ai.iter().map(|x| x.cpu).sum::<f64>(),
                   ai.iter().map(|x| x.cpu_watts).sum::<f64>(), size(ai.iter().map(|x| x.memory).sum()), r.gpu_all, r.window_s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GGUF v3 header: one u32 entry, one string array, then `general.name`.
    fn gguf(name: &str) -> Vec<u8> {
        let s = |v: &mut Vec<u8>, x: &str| {
            v.extend((x.len() as u64).to_le_bytes());
            v.extend(x.as_bytes());
        };
        let mut h = b"GGUF".to_vec();
        h.extend(3u32.to_le_bytes());
        h.extend(0u64.to_le_bytes());
        h.extend(3u64.to_le_bytes());
        s(&mut h, "general.alignment");
        h.extend(4u32.to_le_bytes());
        h.extend(32u32.to_le_bytes());
        s(&mut h, "general.tags");
        h.extend(9u32.to_le_bytes());
        h.extend(8u32.to_le_bytes());
        h.extend(2u64.to_le_bytes());
        s(&mut h, "chat");
        s(&mut h, "code");
        s(&mut h, "general.name");
        h.extend(8u32.to_le_bytes());
        s(&mut h, name);
        h
    }

    #[test]
    fn finds_models_by_name_and_content() {
        let h = gguf("Qwen2.5 3B Instruct");
        assert_eq!(gguf_name(&h).as_deref(), Some("Qwen2.5 3B Instruct"));
        assert_eq!(gguf_name(&h[..h.len() - 3]), None, "a cut header is not a name");

        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, head: &[u8], len: usize| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let mut b = head.to_vec();
            b.resize(len.max(head.len()), 0);
            std::fs::write(&p, b).unwrap();
            p
        };
        let mb = 2 << 20;
        // Ollama blobs have no extension: GGUF by content, named from the header
        let blob = write("blobs/sha256-5ee4", &h, mb);
        assert_eq!(model_file(&blob).unwrap().1, "Qwen2.5 3B Instruct (GGUF 2 MB)");
        let mut st = 40u64.to_le_bytes().to_vec();
        st.push(b'{');
        let hf = write("hub/models--Qwen--Qwen2.5-3B/blobs/9f1c", &st, mb);
        assert_eq!(model_file(&hf).unwrap().1, "Qwen/Qwen2.5-3B (safetensors 2 MB)");
        let ml = write("Spotlight.bundle/spotlight_l2.mlmodelc/_B0000.DAT", b"", 10);
        assert_eq!(model_file(&ml).unwrap().1, "spotlight_l2 (Core ML)");
        assert_eq!(model_file(&write("x.onnx", b"", 10)).unwrap().1, "x.onnx (ONNX 0 KB)");
        assert!(model_file(&write("tiny", &h, 4096)).is_none(), "magic in a small file is not weights");
        assert!(model_file(&write("libfoo", b"\xcf\xfa\xed\xfe", mb)).is_none(), "a large binary is not a model");
        assert!(model_file(tmp.path()).is_none(), "directories are not files");
    }
}
