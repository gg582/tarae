//! Offers to download something (like IntelliJ's "install plugin" notice) — a card at the bottom right of
//! the editing area, `y` download · `n` not now (normal mode, mouse too — insert mode is not intercepted).
//! While downloading it shows a wave; on failure, the reason + `y` retry · `n` close.
//! What gets downloaded is `What` — grammars (grammar.rs) · the java-debug plugin (java.rs).
//! Drawn by `term::OfferCard`.
//!
//! **Multiple offers stack** (`Editor::offers`, arrival order): only the front one takes keys; the ones
//! behind overlap one line each above the front card, showing only their title — so none is missed.
//! New offers go to the back (so a `y` never lands on the wrong card if the front one changes
//! mid-answer). A card whose download started with `y` moves to the back and shows the wave while
//! downloading; the next one comes forward.
//! On failure it stays in place and asks again when its turn comes. Clicking a back card brings it forward.

use std::path::PathBuf;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Offer {
    /// Id used to find this card when the download finishes.
    pub id: u64,
    /// Bold first line ("Rust syntax colors").
    pub title: String,
    /// Prompt text ("Download and build the tree-sitter grammar?").
    pub question: String,
    /// Text while downloading ("Fetching and building…").
    pub working: String,
    /// Failure title ("Couldn't build the Rust grammar").
    pub failed: String,
    pub what: What,
    pub state: OfferState,
}

#[derive(Clone, Debug)]
pub enum What {
    /// Grammars (the language + ones used with it). `lang` = human-readable language name ·
    /// `then` = work to resume once downloaded.
    Grammars { lang: String, names: Vec<String>, then: Option<Resume> },
    /// java-debug plugin — once downloaded, resumes debugging this file
    /// (`attach` = the address, if it was attaching to a running JVM).
    JavaDebug { file: PathBuf, root: PathBuf, attach: Option<(String, u16)> },
}

/// Work that stopped for lack of a grammar — resumed once downloaded
/// (language features only once the grammar is in).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resume {
    Test(crate::testing::Scope),
    TestDebug,
    Debug,
    Attach(crate::attach::AttachTarget),
}

#[derive(Clone, Debug)]
pub enum OfferState {
    Asking,
    Installing(Instant),
    Failed(String),
}

impl Offer {
    /// A new offer (asking) — `Editor::push_offer` assigns the id.
    pub fn new(title: String, question: &str, working: &str, failed: String, what: What) -> Offer {
        Offer {
            id: 0,
            title,
            question: question.into(),
            working: working.into(),
            failed,
            what,
            state: OfferState::Asking,
        }
    }

    pub fn installing(&self) -> bool {
        matches!(self.state, OfferState::Installing(_))
    }
}

impl crate::editor::Editor {
    /// Resumes the stopped work (after the grammar is downloaded).
    pub fn resume(&mut self, r: Resume) {
        match r {
            Resume::Test(scope) => self.test_run(scope),
            Resume::TestDebug => self.test_debug(),
            Resume::Debug => self.dap_launch(),
            Resume::Attach(t) => self.attach(t),
        }
    }

    /// Stacks an offer (at the back). If one with the same title exists, keeps it (untouched while
    /// downloading; only its content is refreshed while asking).
    pub fn push_offer(&mut self, mut o: Offer) {
        if let Some(old) = self.offers.iter_mut().find(|x| x.title == o.title) {
            if !old.installing() {
                (old.what, old.state) = (o.what, OfferState::Asking);
            }
            return;
        }
        self.offer_seq += 1;
        o.id = self.offer_seq;
        self.offers.push(o);
    }

    /// Normal-mode keys while an offer is shown — true if handled
    /// (`y` download · `n`/Esc dismiss, on the front card).
    pub fn offer_key(&mut self, key: crate::key::Key) -> bool {
        use crate::key::Code;
        let Some(o) = self.offers.first() else { return false };
        if o.installing() || key.ctrl || key.alt {
            return false;
        }
        match key.code {
            Code::Char('y') => self.offer_accept(),
            Code::Char('n') | Code::Esc => self.offer_dismiss(),
            _ => return false,
        }
        true
    }

    /// Downloads the front card — it moves to the back while downloading (the next offer comes forward).
    pub fn offer_accept(&mut self) {
        if self.offers.first().is_none_or(Offer::installing) {
            return;
        }
        let mut o = self.offers.remove(0);
        o.state = OfferState::Installing(Instant::now());
        let (id, what) = (o.id, o.what.clone());
        self.offers.push(o);
        match what {
            What::Grammars { names, .. } => {
                self.install_grammars(names, Some(id));
            }
            What::JavaDebug { .. } => self.java_debug_download(id),
        }
    }

    pub fn offer_dismiss(&mut self) {
        if self.offers.first().is_none_or(Offer::installing) {
            return;
        }
        let o = self.offers.remove(0);
        match o.what {
            What::Grammars { lang, names, .. } => {
                let first = names.first().cloned().unwrap_or_default();
                self.note(format!("No colors for {lang} this session — :grammar-install {first} anytime"));
            }
            What::JavaDebug { .. } => self.note("Java debugging needs java-debug — F5 asks again"),
        }
    }

    /// Brings the `i`-th stacked card to the front (when a back card is clicked).
    pub fn offer_to_front(&mut self, i: usize) {
        if i > 0 && i < self.offers.len() {
            let o = self.offers.remove(i);
            self.offers.insert(0, o);
        }
    }

    /// That offer (when its download finishes — None if it was closed meanwhile).
    pub fn offer_by_id(&self, id: u64) -> Option<&Offer> {
        self.offers.iter().find(|o| o.id == id)
    }

    /// Download finished — on success the card goes away, on failure it shows the reason
    /// (and asks again when its turn comes).
    pub fn offer_finished(&mut self, id: u64, result: Result<(), String>) {
        let Some(i) = self.offers.iter().position(|o| o.id == id) else { return };
        match result {
            Ok(()) => {
                self.offers.remove(i);
            }
            Err(why) => self.offers[i].state = OfferState::Failed(why),
        }
    }

    /// Is anything downloading (keeps the tick running — the wave).
    pub fn offer_busy(&self) -> bool {
        self.offers.iter().any(Offer::installing)
    }
}
