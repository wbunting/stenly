//! Opt-in real-model smoke test: STENLY_TEST_MODELS=<installed models dir>
//! STENLY_TEST_WAV=<synthetic 16 kHz WAV> cargo test --release -p stenly-cli --test speech_worker
use std::{path::PathBuf, sync::Arc, time::Duration};
use stenly_asr::{DICTATION_ASR, ParakeetTranscriber};
fn children() -> Vec<u32> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid,ppid,comm"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            let pid: u32 = cols.next()?.parse().ok()?;
            let parent: u32 = cols.next()?.parse().ok()?;
            (parent == std::process::id()
                && cols
                    .collect::<Vec<_>>()
                    .join(" ")
                    .ends_with("stenly-speech"))
            .then_some(pid)
        })
        .collect()
}
#[test]
fn real_worker_shares_unloads_restarts_and_recovers() {
    let Some(models) = std::env::var_os("STENLY_TEST_MODELS").map(PathBuf::from) else {
        eprintln!("real-model smoke skipped: set STENLY_TEST_MODELS");
        return;
    };
    let wav = PathBuf::from(std::env::var_os("STENLY_TEST_WAV").expect("set STENLY_TEST_WAV"));
    let audio = stenly_core::audio::read_wav_16k_mono(&wav).unwrap();
    let cfg = stenly_core::config::AsrConfig::default();
    if let Ok(mode) = std::env::var("STENLY_TEST_SHUTDOWN") {
        let mut model = ParakeetTranscriber::new(&cfg, &models).unwrap();
        if mode == "progress" || mode == "final_progress" {
            let mut callbacks = 0usize;
            let result = stenly_core::transcript::Transcriber::transcribe(
                &mut model,
                &audio,
                stenly_core::platform::Channel::Mic,
                &mut |_| {
                    callbacks += 1;
                    if mode == "progress" || callbacks >= audio.len().div_ceil(16_000) + 1 {
                        stenly_asr::request_shutdown();
                    }
                },
            );
            assert!(
                result.is_err(),
                "shutdown in progress must not persist a partial success"
            );
        } else {
            let mut live = model.live(stenly_core::platform::Channel::Mic).unwrap();
            live.push(&audio);
            stenly_asr::request_shutdown();
            live.finish();
            assert!(
                model.check_health().is_err(),
                "shutdown between push/finish must not mark live complete"
            );
        }
        return;
    }
    for mode in ["progress", "between_requests", "final_progress"] {
        assert!(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "real_worker_shares_unloads_restarts_and_recovers",
                    "--nocapture"
                ])
                .env("STENLY_TEST_SHUTDOWN", mode)
                .status()
                .unwrap()
                .success()
        );
    }

    DICTATION_ASR.set_idle_timeout(Some(Duration::from_millis(200)));
    let dict = DICTATION_ASR.get(&cfg, &models).unwrap();
    assert_eq!(children().len(), 1);
    let pid = children()[0];
    let meeting = ParakeetTranscriber::new(&cfg, &models).unwrap();
    assert_eq!(
        children(),
        [pid],
        "dictation and meeting should share one process"
    );
    assert_eq!(dict.transcribe_clip(&audio), "Send it tomorrow.");
    dict.check_health().unwrap();
    // An active lease protects the worker even while there is no new audio.
    std::thread::sleep(Duration::from_millis(1300));
    assert_eq!(children(), [pid]);
    drop(dict);
    drop(meeting);
    std::thread::sleep(Duration::from_millis(1600));
    assert!(
        children().is_empty(),
        "idle helper should exit and be reaped"
    );
    let dict = DICTATION_ASR.get(&cfg, &models).unwrap();
    let restarted = children()[0];
    assert_ne!(restarted, pid);
    assert_eq!(dict.transcribe_clip(&audio), "Send it tomorrow.");
    // Exercise the real live meeting channel in the worker.
    let mut live = dict.live(stenly_core::platform::Channel::Mic).unwrap();
    let mut segments = vec![];
    for chunk in audio.chunks(320) {
        segments.extend(live.push(chunk));
    }
    segments.extend(live.finish());
    dict.check_health().unwrap();
    assert!(
        segments
            .iter()
            .any(|s| s.text.to_lowercase().contains("send it tomorrow")),
        "{segments:?}"
    );
    // A killed helper must report failure, then recover on the next lease.
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &restarted.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let _ = dict.transcribe_clip(&audio);
    assert!(dict.check_health().is_err());
    drop(dict);
    let recovered = DICTATION_ASR.get(&cfg, &models).unwrap();
    assert_eq!(recovered.transcribe_clip(&audio), "Send it tomorrow.");
    recovered.check_health().unwrap();
    // Always-ready ignores idle, then disabling releases only the cache lease.
    DICTATION_ASR.set_idle_timeout(None);
    let pid = children()[0];
    drop(recovered);
    std::thread::sleep(Duration::from_millis(1300));
    assert_eq!(children(), [pid]);
    let lease = DICTATION_ASR.get(&cfg, &models).unwrap();
    assert_eq!(Arc::strong_count(&lease), 2);
    DICTATION_ASR.unload();
    assert_eq!(children(), [pid]);
    drop(lease);
    assert!(children().is_empty());

    // Cold streaming captures/queues the opening audio before model readiness.
    let (c, m) = (cfg.clone(), models.clone());
    let stream = stenly_asr::stream::LiveDictation::start(
        move || DICTATION_ASR.get(&c, &m),
        stenly_asr::stream::StreamParams::PAUSE,
    );
    let tx = stream.sender();
    for chunk in audio.chunks(320) {
        tx.send(chunk.to_vec()).unwrap();
    }
    drop(tx);
    let (text, _) = stream.finish().unwrap();
    assert_eq!(text, "Send it tomorrow.");
    let dict = DICTATION_ASR.get(&cfg, &models).unwrap();
    assert!(!dict.decode_words(&audio).is_empty());
    dict.check_health().unwrap();

    // Speaker embeddings also stay in the worker, not in the app.
    let medium = wav.with_file_name("medium.wav");
    let speech = stenly_core::audio::read_wav_16k_mono(&medium).unwrap();
    let seg = stenly_core::transcript::Segment::new(
        stenly_core::platform::Channel::Mic,
        0.,
        speech.len() as f64 / 16_000.,
        "test",
    );
    let asr_pid_before_embedding = children()[0];
    let mut speaker_model = ParakeetTranscriber::new(&cfg, &models).unwrap();
    let embeddings =
        stenly_core::transcript::Transcriber::embed_segments(&mut speaker_model, &[seg], &speech)
            .unwrap();
    assert!(embeddings[0].as_ref().is_some_and(|v| !v.is_empty()));
    let mut active = dict.live(stenly_core::platform::Channel::Mic).unwrap();
    active.push(&speech);
    let speaker_pid = children()
        .into_iter()
        .find(|pid| *pid != asr_pid_before_embedding)
        .expect("speaker directly owned by app");
    assert!(
        std::process::Command::new("kill")
            .args(["-STOP", &speaker_pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    drop(speaker_model);
    assert!(!children().contains(&speaker_pid));
    assert!(!active.finish().is_empty());
    dict.check_health().unwrap();
    // Selecting a short voice sample from a >2h20m recording should not send
    // the whole half-gigabyte WAV through the pipe.
    let mut long_audio = vec![0.; 16_000 * 8500];
    long_audio[..speech.len()].copy_from_slice(&speech);
    let seg = stenly_core::transcript::Segment::new(
        stenly_core::platform::Channel::Mic,
        0.,
        speech.len() as f64 / 16_000.,
        "test",
    );
    let mut speaker_model = ParakeetTranscriber::new(&cfg, &models).unwrap();
    let values = stenly_core::transcript::Transcriber::embed_segments(
        &mut speaker_model,
        &[seg],
        &long_audio,
    )
    .unwrap();
    assert!(values[0].is_some());
    drop(long_audio);
    drop(speaker_model);

    // A corrupt optional speaker model must return an operation error without
    // killing another active live channel on that same worker.
    #[cfg(unix)]
    {
        let broken =
            std::env::temp_dir().join(format!("stenly-speaker-error-{}", std::process::id()));
        std::fs::create_dir_all(&broken).unwrap();
        for name in [
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
            "silero_vad_v5.onnx",
        ] {
            std::os::unix::fs::symlink(models.join(name), broken.join(name)).unwrap();
        }
        std::fs::write(
            broken.join("nemo_en_titanet_small.onnx"),
            "not an ONNX model",
        )
        .unwrap();
        let mut optional = ParakeetTranscriber::new(&cfg, &broken).unwrap();
        let mut channel = optional.live(stenly_core::platform::Channel::Mic).unwrap();
        channel.push(&speech);
        let seg = stenly_core::transcript::Segment::new(
            stenly_core::platform::Channel::Mic,
            0.,
            speech.len() as f64 / 16_000.,
            "test",
        );
        assert!(
            stenly_core::transcript::Transcriber::embed_segments(&mut optional, &[seg], &speech)
                .is_err()
        );
        optional.check_health().unwrap();
        assert!(
            !channel.finish().is_empty(),
            "optional speaker failure destroyed the live channel"
        );
        assert_eq!(optional.transcribe_clip(&audio), "Send it tomorrow.");
        drop(optional);
        std::fs::remove_dir_all(&broken).unwrap();
    }

    // Invalid setup returns a useful error rather than silently falling back
    // to model loading in the UI process.
    let invalid = stenly_core::config::AsrConfig {
        model: "not-a-model".into(),
        ..cfg.clone()
    };
    assert!(ParakeetTranscriber::new(&invalid, &models).is_err());

    // Shutdown must interrupt a worker frozen mid-request without waiting for
    // its 120-second response timeout or blocking on the cache/pool lock.
    let frozen = children()[0];
    assert!(
        std::process::Command::new("kill")
            .args(["-STOP", &frozen.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let pending = std::thread::spawn(move || {
        let _ = dict.transcribe_clip(&audio);
        assert!(dict.check_health().is_err());
    });
    std::thread::sleep(Duration::from_millis(100));
    let start = std::time::Instant::now();
    stenly_asr::shutdown_and_wait();
    assert!(start.elapsed() < Duration::from_secs(2));
    pending.join().unwrap();
    assert!(children().is_empty());
}
