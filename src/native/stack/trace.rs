//! The stack's own motion-lint trace, written beside the capsule's (`<trace>-stack.jsonl` for
//! `SCRAWL_MOTION_TRACE=<trace>.jsonl`). The stack steps on its display link, not with the
//! capsule's frames, so sharing the capsule's rows would stamp its boxes with the wrong times.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

pub struct StackTrace {
    out: std::io::BufWriter<std::fs::File>,
    start: Instant,
    last_t: Option<f64>,
    ids: HashMap<String, u32>,
    states: HashMap<u32, String>,
}

/// One box: key, rect in global top-left points, opacity, corner radius, parent key, clip.
pub type Element = (String, [f32; 4], f32, f32, Option<&'static str>, Option<[f32; 4]>);

fn r2(v: f32) -> f64 {
    (f64::from(v) * 100.0).round() / 100.0
}

impl StackTrace {
    pub fn open(launch: &str, display: [f32; 2]) -> Option<Self> {
        let base = PathBuf::from(std::env::var_os("SCRAWL_MOTION_TRACE")?);
        let stem = base.file_stem()?.to_string_lossy().into_owned();
        let path = base.with_file_name(format!("{stem}-stack.jsonl"));
        let file = std::fs::File::create(&path).ok()?;
        let mut out = std::io::BufWriter::new(file);
        let header = serde_json::json!({
            "type": "header", "launch": launch, "width": display[0], "height": display[1],
            "hz": 60, "scale": 2, "wall": crate::now_epoch_ms() as f64 / 1000.0,
            "platform": "gpui", "units": "pt", "clock": "wall",
        });
        writeln!(out, "{header}").ok()?;
        Some(Self {
            out,
            start: Instant::now(),
            last_t: None,
            ids: HashMap::new(),
            states: HashMap::new(),
        })
    }

    /// One frame: what the stack drew this tick. Unchanged boxes are left out and missing ones
    /// are written as gone, as motion-lint reads them.
    pub fn frame(&mut self, elements: &[Element], events: Vec<String>) {
        let t = self.start.elapsed().as_secs_f64() * 1000.0;
        let n = self
            .last_t
            .map_or(1, |last| ((t - last) / (1000.0 / 60.0)).round().max(1.0) as u64);
        self.last_t = Some(t);
        let mut metas = Vec::new();
        let mut changed = Vec::new();
        let mut seen = HashSet::new();
        for (key, [x, y, w, h], a, r, parent, clip) in elements {
            let next = self.ids.len() as u32 + 1;
            let id = *self.ids.entry(key.clone()).or_insert(next);
            if id == next {
                let mut meta = serde_json::json!({ "id": id, "cls": "calayer", "nm": key, "host": "stack" });
                let suffix = key.split(':').nth(1).unwrap_or("");
                if let Some(pv) = parent.and_then(|p| self.ids.get(&format!("{p}:{suffix}"))) {
                    meta["pv"] = (*pv).into();
                }
                metas.push(meta);
            }
            seen.insert(id);
            let mut e = vec![f64::from(id), r2(*x), r2(*y), r2(*w), r2(*h), (f64::from(*a) * 1000.0).round() / 1000.0, r2(*r), 0.0];
            if let Some([cx, cy, cw, ch]) = clip {
                let x0 = x.max(*cx);
                let y0 = y.max(*cy);
                let x1 = (x + w).min(cx + cw);
                let y1 = (y + h).min(cy + ch);
                let vis = [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)];
                if (vis[0] - x).abs() > 0.25 || (vis[1] - y).abs() > 0.25 || (vis[2] - w).abs() > 0.25 || (vis[3] - h).abs() > 0.25 {
                    e.extend(vis.map(r2));
                }
            }
            let e = serde_json::json!(e);
            let state = e.to_string();
            if self.states.get(&id) != Some(&state) {
                self.states.insert(id, state);
                changed.push(e);
            }
        }
        let gone: Vec<u32> = self.states.keys().copied().filter(|id| !seen.contains(id)).collect();
        for id in &gone {
            self.states.remove(id);
        }
        let mut line = serde_json::json!({ "t": (t * 1000.0).round() / 1000.0, "n": n });
        if !events.is_empty() {
            line["ev"] = events.into();
        }
        if !metas.is_empty() {
            line["m"] = metas.into();
        }
        if !changed.is_empty() {
            line["e"] = changed.into();
        }
        if !gone.is_empty() {
            line["g"] = gone.into();
        }
        let _ = writeln!(self.out, "{line}");
        // The lab ends with `process::exit`, so nothing waits for a drop.
        let _ = self.out.flush();
    }
}
