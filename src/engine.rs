use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use serde_json::json;

use crate::config::Config;
use crate::herdr::{AgentStatus, CompletionOutcome, HerdrClient, ResponseCapture, ResponseSource};
use crate::profile::{actions_for_screen, Completion, Profile, RecipeAction};
use crate::rundir::{
    create_run_dir, persist_prompt, persist_steer_prompt, response_path, write_meta, PromptInput,
    RunMeta, RunMetaTurn,
};
use crate::store::{AgentRow, EventRow, IdKind, Store, TurnRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Tui,
    Exec,
}

impl RunMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Exec => "exec",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub mode: RunMode,
    pub model: String,
    pub effort: String,
    pub cwd: PathBuf,
    pub timeout_s: u64,
    pub keep: bool,
    pub prompt: String,
    pub wait: bool,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub agent: AgentRow,
    pub turn: TurnRow,
    pub response: Option<ResponseCapture>,
}

pub struct Engine<'a> {
    config: &'a Config,
    store: &'a mut Store,
    herdr: HerdrClient,
}

impl<'a> Engine<'a> {
    pub fn new(config: &'a Config, store: &'a mut Store, herdr: HerdrClient) -> Self {
        Self {
            config,
            store,
            herdr,
        }
    }

    pub fn run(&mut self, profile: &dyn Profile, request: RunRequest) -> Result<RunResult> {
        let parent_id = request.parent_id.or_else(|| env::var("ORCR_ID").ok());
        let depth = self.admit(parent_id.as_deref())?;
        let id = self.store.allocate_id(IdKind::Agent)?;
        let run_dir = create_run_dir(&self.config.store_root, &id)?;
        let created_at = Utc::now().to_rfc3339();
        let mut agent = AgentRow::new(
            id.clone(),
            request.name.clone(),
            request.mode.as_str(),
            profile.harness(),
            created_at.clone(),
            run_dir.display().to_string(),
        );
        agent.parent_id = parent_id;
        agent.model = request.model.clone();
        agent.effort = request.effort.clone();
        agent.host = env::var("HOSTNAME").unwrap_or_default();
        agent.herdr_session = self.config.herdr.session.clone();
        agent.cwd = request.cwd.display().to_string();
        agent.keep = request.keep;
        agent.timeout_s = i64::try_from(request.timeout_s).unwrap_or(i64::MAX);
        agent.status = "queued".to_string();
        self.store.create_agent(&agent)?;
        self.event("agent.queued", &id, json!({"depth": depth}))?;

        self.store
            .update_agent_status(&id, "starting", None, None)?;
        self.event("agent.starting", &id, json!({}))?;
        self.herdr.ensure_session(Duration::from_secs(10))?;

        let response = response_path(&run_dir, 1);
        let envs = self.launch_env(&id, agent.parent_id.as_deref(), depth, &response);
        let argv = match request.mode {
            RunMode::Tui => profile.launch_argv(&request.model, &request.effort, true),
            RunMode::Exec => profile
                .exec_argv(&request.model, &request.effort, &request.prompt)
                .ok_or_else(|| anyhow!("exec mode is unsupported for {}", profile.harness()))?,
        };
        let started = self
            .herdr
            .agent_start(&id, &request.cwd, &envs, &argv)
            .with_context(|| format!("failed to launch pane for {id}"))?;
        let session_kind = started
            .agent_session
            .as_ref()
            .and_then(|s| s.kind.as_deref());
        let session_value = started
            .agent_session
            .as_ref()
            .and_then(|s| s.value.as_deref());
        self.store.update_agent_launch(
            &id,
            "starting",
            Some(&started.pane_id),
            started.terminal_id.as_deref(),
            session_kind,
            session_value,
        )?;
        agent.pane_id = Some(started.pane_id.clone());
        agent.terminal_id = started.terminal_id.clone();
        agent.agent_session_kind = session_kind.map(ToString::to_string);
        agent.agent_session_value = session_value.map(ToString::to_string);

        if profile.harness() == "mock" {
            self.herdr
                .wait_output(&started.pane_id, "MOCK_READY", false, 10_000)?;
        }
        self.run_startup_recipe(profile, &started.pane_id)?;

        let prompt_path = persist_prompt(&run_dir, 1, PromptInput::Inline(&request.prompt))?;
        let prompt_paths = serde_json::to_string(&vec![prompt_path.display().to_string()])?;
        let turn = TurnRow::new(
            id.clone(),
            1,
            prompt_paths,
            response.display().to_string(),
            Utc::now().to_rfc3339(),
        );
        self.store.create_turn(&turn)?;
        self.event(
            "turn.prompt",
            &id,
            json!({"turn": 1, "path": prompt_path.display().to_string()}),
        )?;

        let delivered = fs::read_to_string(&prompt_path)?;
        self.herdr.send_input(&started.pane_id, &delivered)?;
        self.store.update_agent_status(&id, "working", None, None)?;
        self.event("agent.working", &id, json!({"turn": 1}))?;

        let capture = if request.wait {
            self.wait_for_turn(profile, &agent, turn.n, request.timeout_s)?
        } else {
            None
        };

        let agent = self
            .store
            .get_agent(&id)?
            .ok_or_else(|| anyhow!("agent disappeared after run: {id}"))?;
        self.write_meta(&agent)?;
        Ok(RunResult {
            agent,
            turn,
            response: capture,
        })
    }

    pub fn steer(&mut self, agent_id: &str, prompt: &str) -> Result<TurnRow> {
        let agent = self
            .store
            .get_agent(agent_id)?
            .ok_or_else(|| anyhow!("agent not found: {agent_id}"))?;
        if agent.status != "working" {
            bail!(
                "state_conflict: agent {agent_id} is {}, wanted working for steer",
                agent.status
            );
        }
        let pane_id = agent
            .pane_id
            .as_deref()
            .ok_or_else(|| anyhow!("agent {agent_id} has no pane_id"))?;
        let mut turns = self.store.list_turns_by_agent(agent_id)?;
        let mut turn = turns
            .pop()
            .ok_or_else(|| anyhow!("agent {agent_id} has no active turn"))?;
        if turn.ended_at.is_some() {
            bail!("state_conflict: latest turn for {agent_id} is already complete");
        }
        let mut prompt_paths: Vec<String> = serde_json::from_str(&turn.prompt_paths)?;
        let steer_index = u32::try_from(prompt_paths.len() + 1).unwrap_or(u32::MAX);
        let path = persist_steer_prompt(
            Path::new(&agent.run_dir),
            u32::try_from(turn.n)?,
            steer_index,
            PromptInput::Inline(prompt),
        )?;
        prompt_paths.push(path.display().to_string());
        turn.prompt_paths = serde_json::to_string(&prompt_paths)?;
        self.store.update_turn(&turn)?;
        let delivered = fs::read_to_string(&path)?;
        self.herdr.send_input(pane_id, &delivered)?;
        self.event(
            "turn.steer",
            agent_id,
            json!({"turn": turn.n, "path": path.display().to_string()}),
        )?;
        Ok(turn)
    }

    pub fn turn(&mut self, agent_id: &str, prompt: &str) -> Result<TurnRow> {
        let agent = self
            .store
            .get_agent(agent_id)?
            .ok_or_else(|| anyhow!("agent not found: {agent_id}"))?;
        if agent.status != "idle" {
            bail!(
                "state_conflict: agent {agent_id} is {}, wanted idle for turn",
                agent.status
            );
        }
        let pane_id = agent
            .pane_id
            .as_deref()
            .ok_or_else(|| anyhow!("agent {agent_id} has no pane_id"))?;
        let next = self
            .store
            .list_turns_by_agent(agent_id)?
            .last()
            .map(|turn| turn.n + 1)
            .unwrap_or(1);
        let run_dir = Path::new(&agent.run_dir);
        let response = response_path(run_dir, u32::try_from(next)?);
        let prompt_path =
            persist_prompt(run_dir, u32::try_from(next)?, PromptInput::Inline(prompt))?;
        let mut turn = TurnRow::new(
            agent_id,
            next,
            serde_json::to_string(&vec![prompt_path.display().to_string()])?,
            response.display().to_string(),
            Utc::now().to_rfc3339(),
        );
        self.store.create_turn(&turn)?;
        let delivered = fs::read_to_string(&prompt_path)?;
        self.herdr.send_input(pane_id, &delivered)?;
        self.store
            .update_agent_status(agent_id, "working", None, None)?;
        self.event(
            "turn.prompt",
            agent_id,
            json!({"turn": next, "path": prompt_path.display().to_string()}),
        )?;
        turn = self
            .store
            .list_turns_by_agent(agent_id)?
            .into_iter()
            .find(|row| row.n == next)
            .ok_or_else(|| anyhow!("turn disappeared: {agent_id}:t{next}"))?;
        Ok(turn)
    }

    pub fn wait_for_agent(
        &mut self,
        profile: &dyn Profile,
        agent_id: &str,
        timeout_s: u64,
    ) -> Result<Option<ResponseCapture>> {
        let agent = self
            .store
            .get_agent(agent_id)?
            .ok_or_else(|| anyhow!("agent not found: {agent_id}"))?;
        let turn_n = self
            .store
            .list_turns_by_agent(agent_id)?
            .last()
            .map(|turn| turn.n)
            .ok_or_else(|| anyhow!("agent {agent_id} has no turns"))?;
        self.wait_for_turn(profile, &agent, turn_n, timeout_s)
    }

    pub fn kill_agent(&mut self, profile: &dyn Profile, agent_id: &str) -> Result<bool> {
        let agent = self
            .store
            .get_agent(agent_id)?
            .ok_or_else(|| anyhow!("agent not found: {agent_id}"))?;
        let Some(pane_id) = agent.pane_id.as_deref() else {
            return Ok(false);
        };
        for step in profile.shutdown_recipe() {
            let _ = self.apply_action(pane_id, &step.action);
        }
        let _ = self.herdr.pane_close(pane_id);
        self.store.update_agent_status(
            agent_id,
            "killed",
            Some("killed"),
            Some(&Utc::now().to_rfc3339()),
        )?;
        self.event("agent.killed", agent_id, json!({}))?;
        if let Some(agent) = self.store.get_agent(agent_id)? {
            self.write_meta(&agent)?;
        }
        Ok(true)
    }

    fn admit(&self, parent_id: Option<&str>) -> Result<u32> {
        let caller_depth = env::var("ORCR_DEPTH")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let depth = if let Some(parent_id) = parent_id {
            if self.store.get_agent(parent_id)?.is_none()
                && env::var("ORCR_ID").ok().as_deref() != Some(parent_id)
            {
                bail!("parent agent not found: {parent_id}");
            }
            caller_depth.saturating_add(1)
        } else {
            0
        };
        if depth > self.config.limits.max_depth {
            bail!(
                "max_depth exceeded: requested depth {depth}, limit {}",
                self.config.limits.max_depth
            );
        }
        if let Some(root) = parent_id {
            let count = self.count_tree_agents(root)?;
            if count >= self.config.limits.max_agents_per_tree {
                bail!(
                    "max_agents_per_tree exceeded for {root}: existing {count}, limit {}",
                    self.config.limits.max_agents_per_tree
                );
            }
        }
        Ok(depth)
    }

    fn count_tree_agents(&self, root: &str) -> Result<u32> {
        let agents = self.store.list_agents()?;
        let mut count = 0_u32;
        let mut frontier = vec![root.to_string()];
        while let Some(parent) = frontier.pop() {
            for agent in agents
                .iter()
                .filter(|a| a.parent_id.as_deref() == Some(&parent))
            {
                count = count.saturating_add(1);
                frontier.push(agent.id.clone());
            }
        }
        Ok(count)
    }

    fn launch_env(
        &self,
        id: &str,
        parent_id: Option<&str>,
        depth: u32,
        response: &Path,
    ) -> Vec<(String, String)> {
        let mut envs = vec![
            ("ORCR_ID".to_string(), id.to_string()),
            ("ORCR_DEPTH".to_string(), depth.to_string()),
            (
                "ORCR_STORE".to_string(),
                self.config.store_root.display().to_string(),
            ),
            ("ORCR_OUT".to_string(), response.display().to_string()),
        ];
        if let Some(parent_id) = parent_id {
            envs.push(("ORCR_PARENT".to_string(), parent_id.to_string()));
        }
        envs
    }

    fn run_startup_recipe(&self, profile: &dyn Profile, pane_id: &str) -> Result<()> {
        let screen = self
            .herdr
            .pane_read(pane_id, Some("recent-unwrapped"), Some(1000), Some("text"))
            .unwrap_or_default();
        for action in actions_for_screen(profile.startup_recipe(), &screen) {
            self.apply_action(pane_id, &action)?;
        }
        Ok(())
    }

    fn apply_action(&self, pane_id: &str, action: &RecipeAction) -> Result<()> {
        match action {
            RecipeAction::SendText(text) => self.herdr.pane_send_text(pane_id, text)?,
            RecipeAction::SendKey(key) => self.herdr.pane_send_keys(pane_id, &[key])?,
        }
        Ok(())
    }

    fn await_completion(
        &self,
        profile: &dyn Profile,
        pane_id: &str,
        timeout_s: u64,
    ) -> Result<CompletionOutcome> {
        let timeout = Duration::from_secs(timeout_s);
        match profile.completion() {
            Completion::StatusTransition => {
                self.herdr.watch_status_completion(pane_id, timeout, None)
            }
            Completion::StatusWithGrace(ms) => {
                self.herdr
                    .watch_status_completion(pane_id, timeout, Some(ms))
            }
            Completion::OutputMarker { done, blocked } => self
                .herdr
                .watch_output_markers(pane_id, &done, &blocked, timeout),
        }
        .map_err(Into::into)
    }

    fn capture_response(
        &self,
        profile: &dyn Profile,
        agent: &AgentRow,
        response_path: &Path,
    ) -> Result<ResponseCapture> {
        if response_path.exists() {
            return Ok(ResponseCapture {
                text: fs::read_to_string(response_path)?,
                source: ResponseSource::File,
            });
        }
        if let (Some(adapter), Some(session_ref)) =
            (profile.transcript(), agent.agent_session_value.as_deref())
        {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            if let Some(text) = adapter.extract_last_response(&home, session_ref)? {
                if let Some(parent) = response_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(response_path, &text)?;
                return Ok(ResponseCapture {
                    text,
                    source: ResponseSource::Transcript,
                });
            }
        }
        let pane_id = agent
            .pane_id
            .as_deref()
            .ok_or_else(|| anyhow!("agent {} has no pane_id for scrape fallback", agent.id))?;
        let text =
            self.herdr
                .pane_read(pane_id, Some("recent-unwrapped"), Some(1000), Some("text"))?;
        if let Some(parent) = response_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(response_path, &text)?;
        Ok(ResponseCapture {
            text,
            source: ResponseSource::Scrape,
        })
    }

    fn wait_for_turn(
        &mut self,
        profile: &dyn Profile,
        agent: &AgentRow,
        turn_n: i64,
        timeout_s: u64,
    ) -> Result<Option<ResponseCapture>> {
        let pane_id = agent
            .pane_id
            .as_deref()
            .ok_or_else(|| anyhow!("agent {} has no pane_id", agent.id))?;
        let outcome = self.await_completion(profile, pane_id, timeout_s)?;
        let mut turn = self
            .store
            .list_turns_by_agent(&agent.id)?
            .into_iter()
            .find(|turn| turn.n == turn_n)
            .ok_or_else(|| anyhow!("turn not found: {}:t{}", agent.id, turn_n))?;
        let outcome =
            if outcome == CompletionOutcome::Timeout && Path::new(&turn.response_path).exists() {
                CompletionOutcome::Done
            } else {
                outcome
            };
        match outcome {
            CompletionOutcome::Done => {
                let capture =
                    self.capture_response(profile, agent, Path::new(&turn.response_path))?;
                turn.response_source = Some(capture.source.as_str().to_string());
                turn.ended_at = Some(Utc::now().to_rfc3339());
                self.store.update_turn(&turn)?;
                let final_status = if agent.keep { "idle" } else { "done" };
                let ended_at = (!agent.keep).then(|| Utc::now().to_rfc3339());
                self.store.update_agent_status(
                    &agent.id,
                    final_status,
                    Some("completed"),
                    ended_at.as_deref(),
                )?;
                if !agent.keep {
                    let _ = self.herdr.pane_close(pane_id);
                }
                self.event(
                    "turn.completed",
                    &agent.id,
                    json!({"turn": turn_n, "response_source": capture.source.as_str()}),
                )?;
                if let Some(agent) = self.store.get_agent(&agent.id)? {
                    self.write_meta(&agent)?;
                }
                Ok(Some(capture))
            }
            CompletionOutcome::Blocked => {
                self.store
                    .update_agent_status(&agent.id, "blocked", Some("blocked"), None)?;
                self.event("agent.blocked", &agent.id, json!({"turn": turn_n}))?;
                Ok(None)
            }
            CompletionOutcome::Timeout => {
                self.store.update_agent_status(
                    &agent.id,
                    "timeout",
                    Some("timeout"),
                    Some(&Utc::now().to_rfc3339()),
                )?;
                self.event("agent.wait_timeout", &agent.id, json!({"turn": turn_n}))?;
                if let Some(agent) = self.store.get_agent(&agent.id)? {
                    self.write_meta(&agent)?;
                }
                Ok(None)
            }
            CompletionOutcome::PaneGone => {
                let ended_at = Utc::now().to_rfc3339();
                self.store.update_agent_status(
                    &agent.id,
                    "lost",
                    Some("pane_gone"),
                    Some(&ended_at),
                )?;
                self.event("agent.lost", &agent.id, json!({"turn": turn_n}))?;
                Ok(None)
            }
        }
    }

    fn write_meta(&self, agent: &AgentRow) -> Result<()> {
        let turns = self.store.list_turns_by_agent(&agent.id)?;
        let meta_turns = turns
            .iter()
            .map(|turn| RunMetaTurn {
                n: turn.n,
                prompt_paths: serde_json::from_str(&turn.prompt_paths).unwrap_or_default(),
                response_path: turn.response_path.clone(),
                response_source: turn.response_source.clone(),
                started_at: turn.started_at.clone(),
                ended_at: turn.ended_at.clone(),
            })
            .collect();
        let meta = RunMeta {
            id: agent.id.clone(),
            name: agent.name.clone(),
            parent_id: agent.parent_id.clone(),
            harness: agent.harness.clone(),
            model: agent.model.clone(),
            status: agent.status.clone(),
            created_at: agent.created_at.parse()?,
            ended_at: agent.ended_at.clone(),
            cwd: Some(agent.cwd.clone()),
            pane_id: agent.pane_id.clone(),
            terminal_id: agent.terminal_id.clone(),
            response_source: turns.last().and_then(|turn| turn.response_source.clone()),
            turns: meta_turns,
        };
        write_meta(Path::new(&agent.run_dir), &meta)?;
        Ok(())
    }

    fn event(&self, kind: &str, ref_id: &str, payload: serde_json::Value) -> Result<()> {
        self.store.append_event(&EventRow::new(
            Utc::now().to_rfc3339(),
            kind,
            Some(ref_id.to_string()),
            payload.to_string(),
        ))?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerTracker {
    seen_working_since_input: bool,
    pending_inputs: u32,
}

impl Default for SteerTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl SteerTracker {
    pub fn new() -> Self {
        Self {
            seen_working_since_input: false,
            pending_inputs: 1,
        }
    }

    pub fn steer_input(&mut self) {
        self.pending_inputs = self.pending_inputs.saturating_add(1);
        self.seen_working_since_input = false;
    }

    pub fn observe(&mut self, status: AgentStatus) -> Option<CompletionOutcome> {
        match status {
            AgentStatus::Working => {
                self.seen_working_since_input = true;
                None
            }
            AgentStatus::Idle if self.seen_working_since_input => Some(CompletionOutcome::Done),
            AgentStatus::Blocked => Some(CompletionOutcome::Blocked),
            AgentStatus::Done => Some(CompletionOutcome::Done),
            AgentStatus::Idle | AgentStatus::Unknown | AgentStatus::Other => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tempfile::tempdir;

    use crate::config::Config;
    use crate::herdr::HerdrClient;
    use crate::profile::mock::mock_profile_with_path;

    use super::*;

    #[test]
    fn steer_tracker_requires_idle_after_last_input() {
        let mut tracker = SteerTracker::new();
        assert_eq!(tracker.observe(AgentStatus::Idle), None);
        assert_eq!(tracker.observe(AgentStatus::Working), None);
        tracker.steer_input();
        assert_eq!(tracker.observe(AgentStatus::Idle), None);
        assert_eq!(tracker.observe(AgentStatus::Working), None);
        assert_eq!(
            tracker.observe(AgentStatus::Idle),
            Some(CompletionOutcome::Done)
        );
    }

    #[test]
    fn engine_runs_against_fake_herdr_and_writes_meta() {
        let temp = tempdir().unwrap();
        let herdr = write_fake_herdr(temp.path());
        let mock_bin = temp.path().join("mock-agent");
        fs::write(&mock_bin, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&mock_bin);

        let mut config = Config::load_from(temp.path()).unwrap();
        config.herdr.session = "orcr-test".to_string();
        let mut store = Store::open(temp.path()).unwrap();
        let client = HerdrClient::new(herdr, config.herdr.session.clone())
            .with_timings(Duration::from_millis(1), Duration::from_millis(1));
        let profile = mock_profile_with_path(mock_bin);
        let mut engine = Engine::new(&config, &mut store, client);

        let result = engine
            .run(
                &profile,
                RunRequest {
                    name: Some("worker".to_string()),
                    parent_id: None,
                    mode: RunMode::Tui,
                    model: String::new(),
                    effort: String::new(),
                    cwd: temp.path().to_path_buf(),
                    timeout_s: 2,
                    keep: false,
                    prompt: "hello".to_string(),
                    wait: true,
                },
            )
            .unwrap();

        assert_eq!(result.agent.id, "a1");
        assert_eq!(result.agent.status, "done");
        assert_eq!(
            result.response.as_ref().unwrap().source,
            ResponseSource::File
        );
        assert_eq!(
            env_file_value(temp.path(), "ORCR_ID"),
            Some("a1".to_string())
        );
        assert!(temp.path().join("runs/a1/001-prompt.md").exists());
        assert!(temp.path().join("runs/a1/001-response.md").exists());
        let meta = fs::read_to_string(temp.path().join("runs/a1/meta.json")).unwrap();
        assert!(meta.contains("\"id\": \"a1\""));
        assert!(meta.contains("\"status\": \"done\""));
        assert_eq!(store.list_events().unwrap().len(), 5);
    }

    #[test]
    fn steer_appends_prompt_path_and_keeps_single_response() {
        let temp = tempdir().unwrap();
        let herdr = write_fake_herdr(temp.path());
        let config = Config::load_from(temp.path()).unwrap();
        let mut store = Store::open(temp.path()).unwrap();
        let run_dir = create_run_dir(temp.path(), "a1").unwrap();
        let response = response_path(&run_dir, 1);
        let first_prompt = persist_prompt(&run_dir, 1, PromptInput::Inline("first")).unwrap();
        let mut agent = AgentRow::new(
            "a1",
            Some("worker".to_string()),
            "tui",
            "mock",
            Utc::now().to_rfc3339(),
            run_dir.display().to_string(),
        );
        agent.status = "working".to_string();
        agent.pane_id = Some("w1:p1".to_string());
        store.create_agent(&agent).unwrap();
        let turn = TurnRow::new(
            "a1",
            1,
            serde_json::to_string(&vec![first_prompt.display().to_string()]).unwrap(),
            response.display().to_string(),
            Utc::now().to_rfc3339(),
        );
        store.create_turn(&turn).unwrap();
        let client = HerdrClient::new(herdr, config.herdr.session.clone())
            .with_timings(Duration::from_millis(1), Duration::from_millis(1));
        let mut engine = Engine::new(&config, &mut store, client);

        let updated = engine.steer("a1", "second").unwrap();
        let prompt_paths: Vec<String> = serde_json::from_str(&updated.prompt_paths).unwrap();

        assert_eq!(prompt_paths.len(), 2);
        assert!(prompt_paths[1].ends_with("001-prompt.2.md"));
        assert_eq!(updated.response_path, response.display().to_string());
        assert!(run_dir.join("001-prompt.2.md").exists());
        assert!(!run_dir.join("001-response.2.md").exists());
    }

    fn write_fake_herdr(root: &Path) -> PathBuf {
        let state = root.join("fake-state");
        fs::create_dir_all(&state).unwrap();
        let script = root.join("fake-herdr.py");
        fs::write(
            &script,
            format!(
                r##"#!/usr/bin/env python3
import json, os, sys
state = {state:?}
args = sys.argv[1:]
if args and args[0] == "--session":
    args = args[2:]
def out(v):
    print(json.dumps({{"result": v}}))
if args == ["--version"]:
    print("fake-herdr 1")
elif args == ["server"]:
    sys.exit(0)
elif args == ["status", "server", "--json"]:
    out({{"status":"running","running":True,"version":"fake","protocol":1,"compatible":True,"socket":None,"session":"orcr-test","restart_needed":False}})
elif args[:2] == ["agent", "start"]:
    envs = [a for i,a in enumerate(args) if args[i-1] == "--env"]
    with open(os.path.join(state, "env.txt"), "w") as f:
        f.write("\n".join(envs))
    out({{"agent":{{"agent_status":"idle","cwd":args[args.index("--cwd")+1],"focused":False,"foreground_cwd":None,"name":args[2],"pane_id":"w1:p1","revision":0,"tab_id":"w1:t1","terminal_id":"term1","workspace_id":"w1","agent_session":None}}}})
elif args[:2] == ["pane", "read"]:
    print("MOCK_READY\nMOCK_DONE\nfake scrape")
elif args[:2] == ["wait", "output"]:
    out({{"matched_line":"MOCK_READY","pane_id":"w1:p1","read":{{"format":"text","pane_id":"w1:p1","revision":1,"source":"recent-unwrapped","text":"MOCK_READY","truncated":False}},"revision":1}})
elif args[:2] == ["pane", "send-text"]:
    text = args[3]
    marker = "file: "
    start = text.find(marker)
    if start >= 0:
        rest = text[start+len(marker):]
        end = rest.find(". Do not consider")
        if end >= 0:
            path = rest[:end]
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as f:
                f.write("# fake response\n" + text)
elif args[:2] == ["pane", "send-keys"]:
    sys.exit(0)
elif args[:2] == ["pane", "get"]:
    count_path = os.path.join(state, "get-count")
    try:
        count = int(open(count_path).read())
    except Exception:
        count = 0
    count += 1
    open(count_path, "w").write(str(count))
    status = "working" if count == 1 else "idle"
    out({{"pane":{{"agent_status":status,"cwd":None,"focused":False,"foreground_cwd":None,"label":"a1","pane_id":"w1:p1","revision":count,"tab_id":"w1:t1","terminal_id":"term1","workspace_id":"w1","agent_session":None}}}})
elif args[:2] == ["pane", "close"]:
    out({{"closed":True}})
else:
    print("unknown args " + repr(args), file=sys.stderr)
    sys.exit(1)
"##,
                state = state.display().to_string()
            ),
        )
        .unwrap();
        make_executable(&script);
        script
    }

    fn make_executable(path: &Path) {
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    fn env_file_value(root: &Path, key: &str) -> Option<String> {
        fs::read_to_string(root.join("fake-state/env.txt"))
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix(&format!("{key}="))
                    .map(ToString::to_string)
            })
    }

    #[allow(dead_code)]
    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
