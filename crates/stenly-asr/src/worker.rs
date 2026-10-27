//! Private length-prefixed pipe protocol: JSON metadata + little-endian f32 audio.
//! No ports, shell, temporary audio files, or in-process inference fallback.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, Once, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use stenly_core::{
    config::AsrConfig,
    platform::Channel,
    transcript::{Segment, TimedWord, Transcriber},
};

#[derive(Serialize, Deserialize)]
enum Request {
    Init { cfg: AsrConfig, models: PathBuf },
    SpeakerInit { models: PathBuf },
    Clip,
    Words,
    LiveStart(Channel),
    LivePush(u64),
    LiveFinish(u64),
    Embed(Vec<Segment>),
}
#[derive(Serialize, Deserialize)]
struct Response {
    value: serde_json::Value,
    error: Option<String>,
}
const MAX_JSON: usize = 16 * 1024 * 1024;
const MAX_AUDIO: usize = 512 * 1024 * 1024;

fn write_frame(w: &mut impl Write, meta: &impl Serialize, audio: &[f32]) -> Result<()> {
    let json = serde_json::to_vec(meta)?;
    anyhow::ensure!(
        json.len() <= MAX_JSON && audio.len() <= MAX_AUDIO / 4,
        "speech request is too large"
    );
    w.write_all(&(json.len() as u32).to_le_bytes())?;
    w.write_all(&((audio.len() * 4) as u32).to_le_bytes())?;
    w.write_all(&json)?;
    // Bounded conversion, rather than another recording-sized allocation.
    let mut bytes = Vec::with_capacity(64 * 1024);
    for chunk in audio.chunks(16 * 1024) {
        bytes.clear();
        for f in chunk {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        w.write_all(&bytes)?;
    }
    w.flush()?;
    Ok(())
}
fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> Result<(T, Vec<f32>)> {
    let mut header = [0u8; 8];
    r.read_exact(&mut header)?;
    let json_len = u32::from_le_bytes(header[..4].try_into()?) as usize;
    let audio_len = u32::from_le_bytes(header[4..].try_into()?) as usize;
    anyhow::ensure!(
        json_len <= MAX_JSON && audio_len <= MAX_AUDIO && audio_len % 4 == 0,
        "invalid speech frame length"
    );
    let mut json = vec![0; json_len];
    r.read_exact(&mut json)?;
    let mut audio = Vec::with_capacity(audio_len / 4);
    let mut bytes = [0u8; 64 * 1024];
    let mut remaining = audio_len;
    while remaining > 0 {
        let n = remaining.min(bytes.len());
        r.read_exact(&mut bytes[..n])?;
        audio.extend(
            bytes[..n]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap())),
        );
        remaining -= n;
    }
    Ok((serde_json::from_slice(&json)?, audio))
}

struct Process {
    requests: std::sync::mpsc::Sender<(Request, Vec<f32>)>,
    replies: std::sync::mpsc::Receiver<Result<Response>>,
    failed: bool,
}
struct Client {
    process: Mutex<Process>,
    child: Mutex<Child>,
    error: Mutex<Option<String>>,
    touched: Mutex<Instant>,
}
impl Drop for Client {
    fn drop(&mut self) {
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Client {
    fn start(cfg: &AsrConfig, models: &Path) -> Result<Arc<Self>> {
        // Downloads are setup, not inference. Do them before the worker's
        // bounded Init request; a slow first-run download may take minutes.
        let spec = crate::models::find(&cfg.model)
            .with_context(|| format!("unknown asr.model {:?}", cfg.model))?;
        crate::models::ensure(spec, models, &mut |_| {})?;
        crate::models::ensure_vad(models)?;
        anyhow::ensure!(
            !STOPPING.load(Ordering::Relaxed),
            "speech recognition is shutting down"
        );
        let client = Self::spawn()?;
        client.call::<()>(
            Request::Init {
                cfg: cfg.clone(),
                models: models.to_path_buf(),
            },
            &[],
        )?;
        tracing::info!(
            pid = client.child.lock().unwrap().id(),
            "speech worker ready"
        );
        Ok(client)
    }
    fn start_speaker(models: &Path) -> Result<Arc<Self>> {
        let client = Self::spawn()?;
        client.call::<()>(
            Request::SpeakerInit {
                models: models.to_path_buf(),
            },
            &[],
        )?;
        Ok(client)
    }
    fn spawn() -> Result<Arc<Self>> {
        let current = std::env::current_exe()?;
        let directory = current.parent().context("no executable directory")?;
        // Cargo integration tests live in target/{profile}/deps; production
        // binaries and packaged helpers are always adjacent.
        let directory = if directory.file_name().is_some_and(|n| n == "deps") {
            directory.parent().context("no Cargo profile directory")?
        } else {
            directory
        };
        let executable = directory.join("stenly-speech");
        let mut child = Command::new(&executable).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()
            .with_context(|| format!("could not start speech worker {}; rebuild/install stenly-speech alongside Stenly", executable.display()))?;
        let mut input = child
            .stdin
            .take()
            .context("speech worker stdin unavailable")?;
        let mut output = child
            .stdout
            .take()
            .context("speech worker stdout unavailable")?;
        let (tx, replies) = std::sync::mpsc::channel();
        let (requests, rx) = std::sync::mpsc::channel::<(Request, Vec<f32>)>();
        // Both pipe writes and reads run off the caller. A frozen worker must
        // not defeat the timeout by blocking a large audio write first.
        std::thread::spawn(move || {
            while let Ok((request, audio)) = rx.recv() {
                let response = write_frame(&mut input, &request, &audio)
                    .and_then(|()| read_frame::<Response>(&mut output).map(|(r, _)| r));
                let failed = response.is_err();
                if tx.send(response).is_err() || failed {
                    break;
                }
            }
        });
        let client = Arc::new(Self {
            process: Mutex::new(Process {
                requests,
                replies,
                failed: false,
            }),
            child: Mutex::new(child),
            error: Mutex::new(None),
            touched: Mutex::new(Instant::now()),
        });
        {
            let mut clients = CLIENTS.get_or_init(Default::default).lock().unwrap();
            clients.retain(|c| c.strong_count() > 0);
            clients.push(Arc::downgrade(&client));
        }
        Ok(client)
    }
    fn call<T: DeserializeOwned>(&self, request: Request, audio: &[f32]) -> Result<T> {
        self.call_timeout(request, audio, Duration::from_secs(120))
    }
    fn call_timeout<T: DeserializeOwned>(
        &self,
        request: Request,
        audio: &[f32],
        timeout: Duration,
    ) -> Result<T> {
        if STOPPING.load(Ordering::Relaxed) {
            bail!("speech recognition is shutting down");
        }
        let mut p = self
            .process
            .lock()
            .map_err(|_| anyhow::anyhow!("speech worker lock poisoned"))?;
        anyhow::ensure!(!p.failed, "speech worker stopped; try dictation again");
        let result = (|| -> Result<Response> {
            p.requests
                .send((request, audio.to_vec()))
                .context("speech worker pipe closed")?;
            let response = p
                .replies
                .recv_timeout(timeout)
                .context("speech worker response deadline exceeded")??;
            Ok(response)
        })();
        *self.touched.lock().unwrap() = Instant::now();
        if let Err(e) = &result {
            *self.error.lock().unwrap() = Some(format!("Speech recognition failed: {e:#}"));
            p.failed = true;
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
        }
        // An operation error is a valid reply, not a dead transport. In
        // particular, optional speaker setup must not destroy live channels.
        let response = result?;
        if let Some(error) = response.error {
            bail!("{error}");
        }
        serde_json::from_value(response.value).context("invalid speech worker reply")
    }
    fn healthy(&self) -> bool {
        self.error.lock().map(|e| e.is_none()).unwrap_or(false)
    }
}
static STOPPING: AtomicBool = AtomicBool::new(false);
static CLIENTS: OnceLock<Mutex<Vec<Weak<Client>>>> = OnceLock::new();
static POOL: OnceLock<Mutex<HashMap<String, Weak<Client>>>> = OnceLock::new();
fn key(cfg: &AsrConfig, models: &Path) -> String {
    format!(
        "{}|{}|{}|{}",
        cfg.model,
        cfg.provider,
        cfg.threads,
        models.display()
    )
}
fn client(cfg: &AsrConfig, models: &Path) -> Result<Arc<Client>> {
    let mut pool = POOL
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("speech pool lock poisoned"))?;
    pool.retain(|_, c| c.strong_count() > 0);
    let k = key(cfg, models);
    if let Some(c) = pool.get(&k).and_then(Weak::upgrade).filter(|c| c.healthy()) {
        return Ok(c);
    }
    let c = Client::start(cfg, models)?;
    pool.insert(k, Arc::downgrade(&c));
    Ok(c)
}

/// A cheap lease on a worker, shared by dictation, meetings and processing.
/// Dropping the last lease terminates and reaps the inference process.
pub struct ParakeetTranscriber {
    client: Arc<Client>,
    models: PathBuf,
    speaker: Mutex<Option<Arc<Client>>>,
}
impl Drop for ParakeetTranscriber {
    fn drop(&mut self) {
        *self.client.touched.lock().unwrap() = Instant::now();
    }
}
impl ParakeetTranscriber {
    pub fn new(cfg: &AsrConfig, models: &Path) -> Result<Self> {
        Ok(Self {
            client: client(cfg, models)?,
            models: models.to_path_buf(),
            speaker: Mutex::new(None),
        })
    }
    fn required<T: DeserializeOwned>(&self, request: Request, audio: &[f32]) -> Result<T> {
        let result = self.client.call(request, audio);
        if let Err(e) = &result {
            *self.client.error.lock().unwrap() = Some(format!("{e:#}"));
        }
        result
    }
    pub fn warm_up(&self) {} // Worker initialization already includes the first decode.
    pub fn check_health(&self) -> Result<()> {
        anyhow::ensure!(
            !STOPPING.load(Ordering::SeqCst),
            "speech recognition is shutting down; will resume next launch"
        );
        match self.client.error.lock().unwrap().as_ref() {
            Some(e) => bail!("{e}"),
            None => Ok(()),
        }
    }
    pub fn transcribe_clip(&self, samples: &[f32]) -> String {
        self.required(Request::Clip, samples).unwrap_or_default()
    }
    pub fn decode_words(&self, samples: &[f32]) -> Vec<crate::stream::Word> {
        self.required(Request::Words, samples).unwrap_or_default()
    }
    pub fn streamer(
        &self,
        params: crate::stream::StreamParams,
    ) -> crate::stream::Streamer<impl FnMut(&[f32]) -> Vec<crate::stream::Word> + '_> {
        crate::stream::Streamer::new(params, move |s| self.decode_words(s))
    }
    pub fn live(&self, channel: Channel) -> Result<LiveChannel> {
        Ok(LiveChannel {
            client: self.client.clone(),
            id: Some(self.client.call(Request::LiveStart(channel), &[])?),
        })
    }
}
pub struct LiveChannel {
    client: Arc<Client>,
    id: Option<u64>,
}
impl LiveChannel {
    pub fn push(&mut self, samples: &[f32]) -> Vec<Segment> {
        let result = self
            .client
            .call(Request::LivePush(self.id.unwrap()), samples);
        if let Err(e) = &result {
            *self.client.error.lock().unwrap() = Some(format!("{e:#}"));
        }
        result.unwrap_or_default()
    }
    pub fn finish(mut self) -> Vec<Segment> {
        let result = self
            .client
            .call(Request::LiveFinish(self.id.take().unwrap()), &[]);
        if let Err(e) = &result {
            *self.client.error.lock().unwrap() = Some(format!("{e:#}"));
        }
        result.unwrap_or_default()
    }
}
impl Drop for LiveChannel {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self
                .client
                .call::<Vec<Segment>>(Request::LiveFinish(id), &[]);
        }
    }
}
impl Transcriber for ParakeetTranscriber {
    fn transcribe(
        &mut self,
        audio: &[f32],
        channel: Channel,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<Segment>> {
        let mut live = self.live(channel)?;
        let mut out = Vec::new();
        for (i, chunk) in audio.chunks(16_000).enumerate() {
            out.extend(live.push(chunk));
            self.check_health()?;
            progress(((i + 1) * 16_000).min(audio.len()) as f32 / audio.len().max(1) as f32);
        }
        out.extend(live.finish());
        self.check_health()?;
        progress(1.);
        self.check_health()?;
        Ok(out)
    }
    fn embed_segments(&mut self, segs: &[Segment], audio: &[f32]) -> Result<Vec<Option<Vec<f32>>>> {
        // The app owns both workers directly: optional speaker crashes/timeouts
        // never kill shared ASR, and shutdown can reap even a frozen speaker.
        crate::speakers::ensure_model(&self.models)?;
        let mut speaker = self
            .speaker
            .lock()
            .map_err(|_| anyhow::anyhow!("speaker worker lock poisoned"))?;
        if speaker.is_none() {
            *speaker = Some(Client::start_speaker(&self.models)?);
        }
        let mut out = Vec::with_capacity(segs.len());
        for batch in embedding_batches(segs, audio) {
            let (segments, samples) = batch?;
            let values = speaker
                .as_ref()
                .unwrap()
                .call::<Vec<Option<Vec<f32>>>>(Request::Embed(segments), &samples);
            match values {
                Ok(values) => out.extend(values),
                Err(e) => {
                    *speaker = None;
                    return Err(e);
                }
            }
        }
        Ok(out)
    }
    fn words(&mut self, seg: &Segment, audio: &[f32]) -> Vec<TimedWord> {
        let a = ((seg.start * 16_000.) as usize).min(audio.len());
        let b = ((seg.end * 16_000.) as usize).min(audio.len());
        if b <= a {
            return vec![];
        }
        let mut clip = audio[a..b].to_vec();
        clip.resize(clip.len() + 16_000 / 3, 0.);
        self.decode_words(&clip)
            .into_iter()
            .map(|w| TimedWord {
                text: w.text,
                start: seg.start + w.start as f64,
                end: seg.start + w.end as f64,
            })
            .collect()
    }
}

/// Pack selected speech only, rebasing segments against a bounded buffer.
/// Long recordings with sparse voice samples never cross the IPC frame cap.
fn embedding_batches<'a>(
    segs: &'a [Segment],
    audio: &'a [f32],
) -> impl Iterator<Item = Result<(Vec<Segment>, Vec<f32>)>> + 'a {
    const MAX_BATCH: usize = 16_000 * 60; // one minute, ~4 MB
    let mut index = 0;
    std::iter::from_fn(move || {
        if index == segs.len() {
            return None;
        }
        let mut packed = Vec::new();
        let mut relative = Vec::new();
        while index < segs.len() {
            let seg = &segs[index];
            let a = ((seg.start.max(0.) * 16_000.) as usize).min(audio.len());
            let b = ((seg.end.max(0.) * 16_000.) as usize)
                .min(audio.len())
                .max(a);
            let len = b - a;
            if len > MAX_AUDIO / 4 {
                index += 1;
                return Some(Err(anyhow::anyhow!(
                    "speaker segment exceeds speech frame limit"
                )));
            }
            if !relative.is_empty() && packed.len() + len > MAX_BATCH {
                break;
            }
            let mut rebased = seg.clone();
            rebased.start = packed.len() as f64 / 16_000.;
            packed.extend_from_slice(&audio[a..b]);
            rebased.end = packed.len() as f64 / 16_000.;
            relative.push(rebased);
            index += 1;
            if relative.len() == 256 {
                break;
            }
        }
        Some(Ok((relative, packed)))
    })
}

struct Cached {
    key: String,
    model: Arc<ParakeetTranscriber>,
}
pub struct SharedTranscriber {
    inner: Mutex<Option<Cached>>,
    idle: Mutex<Option<Duration>>,
}
impl SharedTranscriber {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            idle: Mutex::new(None),
        }
    }
    /// None = always ready; Some(timeout) = unload after the last activity.
    pub fn set_idle_timeout(&self, timeout: Option<Duration>) {
        *self.idle.lock().unwrap() = timeout;
    }
    pub fn get(&self, cfg: &AsrConfig, models: &Path) -> Result<Arc<ParakeetTranscriber>> {
        static REAPER: Once = Once::new();
        REAPER.call_once(|| {
            std::thread::spawn(|| {
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    DICTATION_ASR.reap();
                    if STOPPING.load(Ordering::Relaxed) {
                        break;
                    }
                }
            });
        });
        let mut g = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("speech cache lock poisoned"))?;
        let k = key(cfg, models);
        if let Some(c) = g
            .as_ref()
            .filter(|c| c.key == k && c.model.client.healthy())
        {
            *c.model.client.touched.lock().unwrap() = Instant::now();
            return Ok(c.model.clone());
        }
        let model = Arc::new(ParakeetTranscriber::new(cfg, models)?);
        *g = Some(Cached {
            key: k,
            model: model.clone(),
        });
        Ok(model)
    }
    fn reap(&self) {
        let Some(timeout) = *self.idle.lock().unwrap() else {
            return;
        };
        // Never wait for a model load on the UI thread or retire an active lease.
        if let Ok(mut g) = self.inner.try_lock() {
            if g.as_ref().is_some_and(|c| {
                Arc::strong_count(&c.model) == 1
                    && Arc::strong_count(&c.model.client) == 1
                    && c.model.client.touched.lock().unwrap().elapsed() >= timeout
            }) {
                *g = None;
                tracing::info!("idle speech worker unloaded");
            }
        }
    }
    pub fn unload(&self) {
        if let Ok(mut g) = self.inner.lock() {
            *g = None;
        }
    }
}
pub static DICTATION_ASR: SharedTranscriber = SharedTranscriber::new();
pub fn request_shutdown() {
    STOPPING.store(true, Ordering::SeqCst);
    // Independent of the model/cache locks: also interrupts a startup or a
    // frozen inference request instead of making UI shutdown wait for it.
    if let Some(clients) = CLIENTS.get() {
        if let Ok(mut clients) = clients.lock() {
            clients.retain(|c| c.strong_count() > 0);
            for c in clients.iter().filter_map(Weak::upgrade) {
                if let Ok(mut child) = c.child.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
}
pub fn shutdown_and_wait() {
    request_shutdown();
    if let Ok(mut cache) = DICTATION_ASR.inner.try_lock() {
        *cache = None;
    }
}

/// Worker entry point. EOF (parent exit) releases every model and exits.
pub fn run_worker() -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let (init, _) = read_frame::<Request>(&mut input)?;
    let result = match init {
        Request::Init { cfg, models } => crate::local::ParakeetTranscriber::new(&cfg, &models),
        Request::SpeakerInit { models } => return run_speaker(&mut input, &mut output, &models),
        _ => bail!("speech worker expected init"),
    };
    let model = match result {
        Ok(m) => {
            m.warm_up();
            Arc::new(m)
        }
        Err(e) => {
            write_frame(
                &mut output,
                &Response {
                    value: serde_json::Value::Null,
                    error: Some(format!("{e:#}")),
                },
                &[],
            )?;
            return Ok(());
        }
    };
    write_frame(
        &mut output,
        &Response {
            value: serde_json::Value::Null,
            error: None,
        },
        &[],
    )?;
    let mut channels = HashMap::new();
    let mut next_id = 0u64;
    loop {
        let (request, audio) = match read_frame::<Request>(&mut input) {
            Ok(r) => r,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let result = (|| -> Result<serde_json::Value> {
            Ok(match request {
                Request::Init { .. } | Request::SpeakerInit { .. } => {
                    bail!("speech worker is already initialized")
                }
                Request::Clip => serde_json::to_value(model.transcribe_clip(&audio))?,
                Request::Words => serde_json::to_value(model.decode_words(&audio))?,
                Request::LiveStart(channel) => {
                    next_id += 1;
                    channels.insert(next_id, model.live(channel)?);
                    serde_json::to_value(next_id)?
                }
                Request::LivePush(id) => serde_json::to_value(
                    channels
                        .get_mut(&id)
                        .context("unknown speech channel")?
                        .push(&audio),
                )?,
                Request::LiveFinish(id) => serde_json::to_value(
                    channels
                        .remove(&id)
                        .context("unknown speech channel")?
                        .finish(),
                )?,
                Request::Embed(_) => bail!("embeddings require a dedicated speaker worker"),
            })
        })();
        let response = match result {
            Ok(value) => Response { value, error: None },
            Err(e) => Response {
                value: serde_json::Value::Null,
                error: Some(format!("{e:#}")),
            },
        };
        write_frame(&mut output, &response, &[])?;
    }
}

/// Native speaker loading can throw a foreign exception on a corrupt ONNX
/// model. Keep that failure domain out of the shared live ASR process.
fn run_speaker(input: &mut impl Read, output: &mut impl Write, models: &Path) -> Result<()> {
    let extractor = crate::speakers::Extractor::new(models)?;
    write_frame(
        output,
        &Response {
            value: serde_json::Value::Null,
            error: None,
        },
        &[],
    )?;
    loop {
        let (request, audio) = match read_frame::<Request>(input) {
            Ok(frame) => frame,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let response = match request {
            Request::Embed(segs) => Response {
                value: serde_json::to_value(extractor.embed(&segs, &audio))?,
                error: None,
            },
            _ => Response {
                value: serde_json::Value::Null,
                error: Some("speaker worker accepts embeddings only".into()),
            },
        };
        write_frame(output, &response, &[])?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_round_trip() {
        let samples = [0., -0.5, 1., f32::MIN_POSITIVE];
        let mut wire = vec![];
        write_frame(&mut wire, &Request::LivePush(42), &samples).unwrap();
        let (req, got) = read_frame::<Request>(&mut wire.as_slice()).unwrap();
        assert!(matches!(req, Request::LivePush(42)));
        assert_eq!(got, samples);
    }
    #[test]
    fn rejects_oversized_or_misaligned_frames() {
        for header in [(u32::MAX, 0u32), (0, 3)] {
            let mut wire = header.0.to_le_bytes().to_vec();
            wire.extend(header.1.to_le_bytes());
            assert!(read_frame::<Request>(&mut wire.as_slice()).is_err());
        }
    }
    #[test]
    fn default_keeps_dictation_ready() {
        assert!(SharedTranscriber::new().idle.lock().unwrap().is_none());
    }
    #[test]
    fn embedding_packs_selected_audio_and_rebases_timestamps() {
        let audio = vec![0.25; 16_000 * 125];
        let segments = vec![
            Segment::new(Channel::Mic, 100., 125., "one"),
            Segment::new(Channel::Mic, 0., 25., "two"),
            Segment::new(Channel::Mic, 25., 50., "three"),
        ];
        let batches: Vec<_> = embedding_batches(&segments, &audio)
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].1.len(), 16_000 * 50);
        assert_eq!(batches[0].0[0].start, 0.);
        assert_eq!(batches[0].0[1].start, 25.);
        assert_eq!(batches[1].0[0].end, 25.);
    }
    #[test]
    fn frozen_optional_worker_timeout_preserves_live_asr() {
        let Some(models) = std::env::var_os("STENLY_TEST_MODELS").map(PathBuf::from) else {
            return;
        };
        let cfg = AsrConfig::default();
        let model = ParakeetTranscriber::new(&cfg, &models).unwrap();
        let mut live = model.live(Channel::Mic).unwrap();
        let speaker = Client::start_speaker(&models).unwrap();
        let pid = speaker.child.lock().unwrap().id();
        assert!(
            std::process::Command::new("kill")
                .args(["-STOP", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            speaker
                .call_timeout::<Vec<Option<Vec<f32>>>>(
                    Request::Embed(vec![]),
                    &[],
                    Duration::from_millis(50)
                )
                .is_err()
        );
        drop(speaker);
        live.push(&vec![0.; 16_000]);
        live.finish();
        model.check_health().unwrap();
    }
}
