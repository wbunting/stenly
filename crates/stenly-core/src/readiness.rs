//! What "ready" means, in one place, for onboarding and Settings.
//!
//! Pure functions over facts the app has already gathered, so the rules are
//! testable without a window, a network or a Keychain:
//!
//! - **Transcription** (the speech model) is the core of the product. It is
//!   ready only when the model's files are on disk and verified; a download
//!   that failed stays visible until it is resolved. Recording, dictation and
//!   buying a licence all depend on it.
//! - **Summaries** are optional. The chosen provider is "ready" only when a
//!   real check passed: Ollama answering with the model installed, the API /
//!   Codex / Claude Code answering a test request, or ChatGPT holding a
//!   signed-in token.

/// State of the speech model that transcription and dictation need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Speech {
    /// Every file is present and verified.
    Ready,
    /// A download is in flight.
    Downloading,
    /// The files are there; their checksums are being checked.
    Verifying,
    /// The last download failed (the message is what went wrong).
    Failed(String),
    /// Nothing downloaded yet.
    Missing,
}

impl Speech {
    /// `installed` = the model's files are on disk and verified. It wins over
    /// everything else: once the files are there, an old failure is moot.
    pub fn from_parts(
        installed: bool,
        downloading: bool,
        verifying: bool,
        failure: Option<&str>,
    ) -> Self {
        if installed {
            Speech::Ready
        } else if downloading {
            Speech::Downloading
        } else if verifying {
            Speech::Verifying
        } else if let Some(f) = failure {
            Speech::Failed(f.to_string())
        } else {
            Speech::Missing
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Speech::Ready)
    }

    /// Short label for a status line.
    pub fn headline(&self) -> &'static str {
        match self {
            Speech::Ready => "Transcription is ready",
            Speech::Downloading => "Downloading the speech model…",
            Speech::Verifying => "Checking the speech model…",
            Speech::Failed(_) => "The speech model didn't download",
            Speech::Missing => "Transcription isn't set up",
        }
    }
}

/// Shown instead of the Buy button while transcription isn't set up.
pub const FINISH_TRANSCRIPTION_FIRST: &str = "Finish setting up transcription first";

/// Paying for a licence is offered only once the product can transcribe.
/// Summaries are optional, so they never gate it.
pub fn purchase_offered(speech: &Speech) -> bool {
    speech.is_ready()
}

/// Recording and dictation both need the speech model.
pub fn can_transcribe(speech: &Speech) -> bool {
    speech.is_ready()
}

/// The ways Stenly can write summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryProvider {
    Ollama,
    Api,
    Codex,
    Claude,
    ChatGpt,
}

/// Outcome of a provider's own test request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Probe {
    #[default]
    Untested,
    Running,
    Ok,
    Failed(String),
}

/// Everything known about the chosen summary provider.
#[derive(Debug, Clone)]
pub struct SummaryFacts {
    /// Settings → Preferences → AI summaries.
    pub enabled: bool,
    pub provider: SummaryProvider,
    /// Ollama has been asked at least once.
    pub ollama_checked: bool,
    pub ollama_running: bool,
    /// The configured model is installed in Ollama.
    pub ollama_model_installed: bool,
    pub ollama_model: String,
    /// API: base URL and model are filled in.
    pub api_configured: bool,
    /// API: a key is saved (or the endpoint is on this Mac and needs none).
    pub api_can_authenticate: bool,
    /// Codex / Claude Code is installed on this Mac.
    pub cli_found: bool,
    /// ChatGPT holds a signed-in token.
    pub chatgpt_signed_in: bool,
    /// The last test of the chosen provider (API, CLI or ChatGPT).
    pub probe: Probe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Summaries {
    /// AI summaries are switched off.
    Off,
    /// Verified: the chosen provider works.
    Ready,
    /// A check is running.
    Checking,
    /// Chosen, but not usable yet; the message says what to do.
    NotReady(String),
}

impl Summaries {
    pub fn is_ready(&self) -> bool {
        matches!(self, Summaries::Ready)
    }
}

impl SummaryProvider {
    pub fn label(self) -> &'static str {
        match self {
            SummaryProvider::Ollama => "Ollama",
            SummaryProvider::Api => "Your API",
            SummaryProvider::Codex => "Codex",
            SummaryProvider::Claude => "Claude Code",
            SummaryProvider::ChatGpt => "ChatGPT",
        }
    }
}

/// Is the chosen provider ready to write summaries?
pub fn summaries(f: &SummaryFacts) -> Summaries {
    use Summaries::*;
    if !f.enabled {
        return Off;
    }
    let probed = |not_run: &str| match &f.probe {
        Probe::Ok => Ready,
        Probe::Running => Checking,
        Probe::Failed(e) => NotReady(e.clone()),
        Probe::Untested => NotReady(not_run.to_string()),
    };
    match f.provider {
        SummaryProvider::Ollama => {
            if !f.ollama_checked {
                Checking
            } else if !f.ollama_running {
                NotReady("Ollama isn't running. Open it, then check again.".into())
            } else if !f.ollama_model_installed {
                NotReady(format!("Download {} to write summaries.", f.ollama_model))
            } else {
                Ready
            }
        }
        SummaryProvider::Api => {
            if !f.api_configured {
                NotReady("Enter the base URL and a model name.".into())
            } else if !f.api_can_authenticate && f.probe == Probe::Untested {
                NotReady("Add your API key, then press Test.".into())
            } else {
                probed("Press Test to check the connection.")
            }
        }
        SummaryProvider::Codex | SummaryProvider::Claude => {
            let name = f.provider.label();
            if !f.cli_found {
                NotReady(format!("{name} isn't installed on this Mac."))
            } else {
                probed(&format!("Press Test to check {name} is signed in."))
            }
        }
        SummaryProvider::ChatGpt => {
            if !f.chatgpt_signed_in {
                NotReady("Sign in with ChatGPT.".into())
            } else {
                match &f.probe {
                    // A signed-in token is the check; a failed request overrides it.
                    Probe::Failed(e) => NotReady(e.clone()),
                    Probe::Running => Checking,
                    Probe::Ok | Probe::Untested => Ready,
                }
            }
        }
    }
}

/// Should the app send the provider's test request by itself now? Never for
/// Ollama (it's checked by listing models) or ChatGPT (the token is the check),
/// and never when it would just fail for a missing key or tool.
pub fn should_auto_probe(f: &SummaryFacts) -> bool {
    if !f.enabled || f.probe != Probe::Untested {
        return false;
    }
    match f.provider {
        SummaryProvider::Api => f.api_configured && f.api_can_authenticate,
        SummaryProvider::Codex | SummaryProvider::Claude => f.cli_found,
        SummaryProvider::Ollama | SummaryProvider::ChatGpt => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(provider: SummaryProvider) -> SummaryFacts {
        SummaryFacts {
            enabled: true,
            provider,
            ollama_checked: true,
            ollama_running: true,
            ollama_model_installed: true,
            ollama_model: "qwen3.5:9b".into(),
            api_configured: true,
            api_can_authenticate: true,
            cli_found: true,
            chatgpt_signed_in: true,
            probe: Probe::Untested,
        }
    }

    #[test]
    fn speech_is_ready_only_when_files_are_installed() {
        assert_eq!(
            Speech::from_parts(false, false, false, None),
            Speech::Missing
        );
        assert_eq!(
            Speech::from_parts(false, true, false, None),
            Speech::Downloading
        );
        assert_eq!(
            Speech::from_parts(false, false, true, None),
            Speech::Verifying
        );
        assert_eq!(
            Speech::from_parts(false, false, false, Some("boom")),
            Speech::Failed("boom".into())
        );
        assert_eq!(Speech::from_parts(true, false, false, None), Speech::Ready);
    }

    #[test]
    fn a_retry_in_flight_hides_the_old_failure_and_success_clears_it() {
        assert_eq!(
            Speech::from_parts(false, true, false, Some("old")),
            Speech::Downloading
        );
        assert_eq!(
            Speech::from_parts(true, false, false, Some("old")),
            Speech::Ready
        );
        // a download that "finished" but left nothing verified is not ready
        assert_eq!(
            Speech::from_parts(false, false, false, None),
            Speech::Missing
        );
    }

    #[test]
    fn purchase_and_dictation_wait_for_transcription() {
        for s in [
            Speech::Missing,
            Speech::Downloading,
            Speech::Verifying,
            Speech::Failed("x".into()),
        ] {
            assert!(!purchase_offered(&s), "{s:?}");
            assert!(!can_transcribe(&s), "{s:?}");
            assert!(!s.headline().is_empty());
        }
        assert!(purchase_offered(&Speech::Ready));
        assert!(can_transcribe(&Speech::Ready));
    }

    #[test]
    fn summaries_never_gate_anything() {
        // Summaries off or broken doesn't change the speech/purchase rules.
        let mut f = facts(SummaryProvider::Ollama);
        f.ollama_running = false;
        assert!(!summaries(&f).is_ready());
        assert!(purchase_offered(&Speech::Ready));
    }

    #[test]
    fn off_when_summaries_are_disabled() {
        let mut f = facts(SummaryProvider::ChatGpt);
        f.enabled = false;
        assert_eq!(summaries(&f), Summaries::Off);
    }

    #[test]
    fn ollama_needs_the_server_and_the_model() {
        let mut f = facts(SummaryProvider::Ollama);
        assert_eq!(summaries(&f), Summaries::Ready);
        f.ollama_checked = false;
        assert_eq!(summaries(&f), Summaries::Checking);
        f.ollama_checked = true;
        f.ollama_running = false;
        assert!(matches!(summaries(&f), Summaries::NotReady(m) if m.contains("isn't running")));
        f.ollama_running = true;
        f.ollama_model_installed = false;
        assert!(matches!(summaries(&f), Summaries::NotReady(m) if m.contains("qwen3.5:9b")));
    }

    #[test]
    fn api_is_ready_only_after_a_passing_test() {
        let mut f = facts(SummaryProvider::Api);
        assert!(matches!(summaries(&f), Summaries::NotReady(m) if m.contains("Test")));
        f.probe = Probe::Running;
        assert_eq!(summaries(&f), Summaries::Checking);
        f.probe = Probe::Failed("401 unauthorized".into());
        assert_eq!(
            summaries(&f),
            Summaries::NotReady("401 unauthorized".into())
        );
        f.probe = Probe::Ok;
        assert_eq!(summaries(&f), Summaries::Ready);
        f.api_configured = false;
        assert!(matches!(summaries(&f), Summaries::NotReady(m) if m.contains("base URL")));
        f.api_configured = true;
        f.api_can_authenticate = false;
        f.probe = Probe::Untested;
        assert!(matches!(summaries(&f), Summaries::NotReady(m) if m.contains("API key")));
    }

    #[test]
    fn cli_tools_need_to_be_installed_and_answer() {
        for p in [SummaryProvider::Codex, SummaryProvider::Claude] {
            let mut f = facts(p);
            assert!(matches!(summaries(&f), Summaries::NotReady(m) if m.contains(p.label())));
            f.probe = Probe::Ok;
            assert_eq!(summaries(&f), Summaries::Ready);
            f.probe = Probe::Failed("not signed in".into());
            assert_eq!(summaries(&f), Summaries::NotReady("not signed in".into()));
            f.cli_found = false;
            f.probe = Probe::Ok;
            assert!(
                matches!(summaries(&f), Summaries::NotReady(m) if m.contains("isn't installed")),
                "a stale pass doesn't count once the tool is gone"
            );
        }
    }

    #[test]
    fn chatgpt_is_ready_with_a_signed_in_token() {
        let mut f = facts(SummaryProvider::ChatGpt);
        assert_eq!(summaries(&f), Summaries::Ready);
        f.chatgpt_signed_in = false;
        assert!(matches!(summaries(&f), Summaries::NotReady(m) if m.contains("Sign in")));
        f.chatgpt_signed_in = true;
        f.probe = Probe::Failed("plan has no Codex access".into());
        assert_eq!(
            summaries(&f),
            Summaries::NotReady("plan has no Codex access".into())
        );
        f.probe = Probe::Running;
        assert_eq!(summaries(&f), Summaries::Checking);
        f.probe = Probe::Ok;
        assert_eq!(summaries(&f), Summaries::Ready);
    }

    #[test]
    fn auto_probe_only_when_it_can_succeed() {
        let mut f = facts(SummaryProvider::Api);
        assert!(should_auto_probe(&f));
        f.api_can_authenticate = false;
        assert!(
            !should_auto_probe(&f),
            "no key: don't send a request that must fail"
        );
        f.api_can_authenticate = true;
        f.probe = Probe::Ok;
        assert!(!should_auto_probe(&f), "already tested");
        let mut c = facts(SummaryProvider::Claude);
        assert!(should_auto_probe(&c));
        c.cli_found = false;
        assert!(!should_auto_probe(&c));
        assert!(!should_auto_probe(&facts(SummaryProvider::ChatGpt)));
        assert!(!should_auto_probe(&facts(SummaryProvider::Ollama)));
        let mut off = facts(SummaryProvider::Api);
        off.enabled = false;
        assert!(!should_auto_probe(&off));
    }
}
