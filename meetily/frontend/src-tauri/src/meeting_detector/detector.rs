//! Core meeting detection logic
//!
//! Provides process monitoring and meeting detection for Zoom, Teams, and Google Meet.

use crate::meeting_detector::meeting_apps::*;
#[cfg(target_os = "macos")]
use log::debug;
use log::{info, warn, error};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::path::PathBuf;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tauri::{AppHandle, Emitter, Runtime, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::RwLock;

/// Represents a detected meeting
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedMeeting {
    /// Name of the meeting application (e.g., "Zoom", "Microsoft Teams", "Google Meet")
    pub app_name: String,
    /// Process name that was detected
    pub process_name: String,
    /// Timestamp when the meeting was detected
    pub detected_at: String,
    /// Whether this is an active meeting (vs just the app running)
    pub is_active_meeting: bool,
}

/// Settings for meeting detection behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingDetectionSettings {
    /// Whether meeting detection is enabled
    pub enabled: bool,
    /// Automatically start recording when a meeting is detected
    pub auto_start_recording: bool,
    /// Automatically stop recording when a meeting ends
    pub auto_stop_recording: bool,
    /// Detect Zoom meetings
    pub detect_zoom: bool,
    /// Detect Microsoft Teams meetings
    pub detect_teams: bool,
    /// Detect Google Meet meetings (requires browser window inspection)
    pub detect_google_meet: bool,
    /// Show notification when a meeting is detected
    pub notify_on_detection: bool,
    /// Polling interval in seconds
    pub poll_interval_secs: u64,
    /// Seconds to wait after the meeting process disappears before treating
    /// the meeting as ended. Absorbs a brief drop-and-rejoin (e.g. a flaky
    /// Zoom connection) without splitting one meeting into two recordings.
    #[serde(default = "default_auto_stop_grace_secs")]
    pub auto_stop_grace_secs: u64,
}

fn default_auto_stop_grace_secs() -> u64 {
    30
}

/// How long after triggering an auto-start to wait for the recording to
/// actually begin before concluding the frontend bailed (model not ready,
/// device error). The frontend's failure UI lives in a window that is
/// normally hidden in the tray, so without this watchdog a failed start is
/// completely silent and leaves the tray stuck on "Starting...".
const AUTO_START_TIMEOUT: Duration = Duration::from_secs(60);

impl MeetingDetectionSettings {
    /// Clamp values that would make the monitor misbehave. A zero poll
    /// interval turns the monitor loop into a 100% CPU busy-loop, and the
    /// settings file is hand-editable, so never trust it.
    fn sanitized(mut self) -> Self {
        if self.poll_interval_secs == 0 {
            self.poll_interval_secs = 1;
        }
        self
    }
}

impl Default for MeetingDetectionSettings {
    fn default() -> Self {
        Self {
            enabled: false, // Opt-in by default for privacy
            auto_start_recording: false,
            auto_stop_recording: true,
            detect_zoom: true,
            detect_teams: true,
            detect_google_meet: true,
            notify_on_detection: true,
            poll_interval_secs: 5,
            auto_stop_grace_secs: default_auto_stop_grace_secs(),
        }
    }
}

impl MeetingDetectionSettings {
    /// Get the settings file path
    fn settings_path() -> Option<PathBuf> {
        dirs::data_dir().map(|p| p.join("com.meetily.ai").join("meeting_detection_settings.json"))
    }

    /// Load settings from disk
    pub fn load() -> Self {
        if let Some(path) = Self::settings_path() {
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => {
                        match serde_json::from_str::<Self>(&contents) {
                            Ok(settings) => {
                                info!("Loaded meeting detection settings from {:?}", path);
                                return settings.sanitized();
                            }
                            Err(e) => {
                                error!("Failed to parse meeting detection settings: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to read meeting detection settings: {}", e);
                    }
                }
            }
        }
        Self::default()
    }

    /// Save settings to disk
    pub fn save(&self) -> Result<(), String> {
        if let Some(path) = Self::settings_path() {
            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create settings directory: {}", e))?;
            }

            let contents = serde_json::to_string_pretty(self)
                .map_err(|e| format!("Failed to serialize settings: {}", e))?;

            std::fs::write(&path, contents)
                .map_err(|e| format!("Failed to write settings: {}", e))?;

            info!("Saved meeting detection settings to {:?}", path);
            Ok(())
        } else {
            Err("Could not determine settings path".to_string())
        }
    }
}

/// Status of the meeting detection monitor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingDetectionStatus {
    /// Whether the monitor is currently running
    pub is_monitoring: bool,
    /// Currently detected meeting, if any
    pub current_meeting: Option<DetectedMeeting>,
    /// Current settings
    pub settings: MeetingDetectionSettings,
    /// Whether recording was auto-started by the detector
    pub auto_recording_active: bool,
}

/// Meeting detector that monitors for video conferencing applications
pub struct MeetingDetector {
    system: System,
    settings: Arc<RwLock<MeetingDetectionSettings>>,
    is_monitoring: Arc<AtomicBool>,
    /// Bumped on every start/stop; a monitor loop exits as soon as the
    /// generation no longer matches the one it was spawned with, so a quick
    /// disable/enable can never leave two loops running.
    monitor_generation: Arc<AtomicU64>,
    current_meeting: Arc<RwLock<Option<DetectedMeeting>>>,
    auto_recording_active: Arc<AtomicBool>,
}

impl MeetingDetector {
    /// Create a new meeting detector with settings loaded from disk
    pub fn new() -> Self {
        // Load persisted settings or use defaults
        let loaded_settings = MeetingDetectionSettings::load();
        info!("MeetingDetector initialized with settings: enabled={}, auto_start={}", 
              loaded_settings.enabled, loaded_settings.auto_start_recording);
        
        Self {
            // Detection only ever reads process names, so skip the expensive
            // per-process cmdline/env/cpu/memory collection.
            system: System::new_with_specifics(
                RefreshKind::new().with_processes(ProcessRefreshKind::new()),
            ),
            settings: Arc::new(RwLock::new(loaded_settings)),
            is_monitoring: Arc::new(AtomicBool::new(false)),
            monitor_generation: Arc::new(AtomicU64::new(0)),
            current_meeting: Arc::new(RwLock::new(None)),
            auto_recording_active: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get current settings
    pub async fn get_settings(&self) -> MeetingDetectionSettings {
        self.settings.read().await.clone()
    }

    /// Update settings and persist to disk
    pub async fn set_settings(&self, settings: MeetingDetectionSettings) {
        let settings = settings.sanitized();
        // Save to disk first
        if let Err(e) = settings.save() {
            error!("Failed to save meeting detection settings: {}", e);
        }
        
        let mut current = self.settings.write().await;
        *current = settings;
    }

    /// Check if monitoring is active
    pub fn is_monitoring(&self) -> bool {
        self.is_monitoring.load(Ordering::SeqCst)
    }

    /// Get current detection status
    pub async fn get_status(&self) -> MeetingDetectionStatus {
        MeetingDetectionStatus {
            is_monitoring: self.is_monitoring(),
            current_meeting: self.current_meeting.read().await.clone(),
            settings: self.get_settings().await,
            auto_recording_active: self.auto_recording_active.load(Ordering::SeqCst),
        }
    }

    /// Detect if any meeting application is running
    pub fn detect_meeting(&mut self, settings: &MeetingDetectionSettings) -> Option<DetectedMeeting> {
        self.system.refresh_processes(ProcessesToUpdate::All, true);

        for (_pid, process) in self.system.processes() {
            let name = process.name().to_string_lossy().to_lowercase();

            // Check for Zoom - only detect ACTIVE meetings, not just the app being open
            if settings.detect_zoom {
                // CptHost is the process that runs during an active Zoom meeting on macOS
                // This is more reliable than just detecting zoom.us which runs when app is open
                if name.contains("cpthost") {
                    info!("Detected active Zoom meeting via CptHost process");
                    return Some(DetectedMeeting {
                        app_name: "Zoom".to_string(),
                        process_name: process.name().to_string_lossy().to_string(),
                        detected_at: chrono::Local::now().to_rfc3339(),
                        is_active_meeting: true,
                    });
                }
            }

            // Check for Microsoft Teams
            if settings.detect_teams {
                for teams_process in TEAMS_PROCESSES {
                    if name.contains(&teams_process.to_lowercase()) {
                        return Some(DetectedMeeting {
                            app_name: "Microsoft Teams".to_string(),
                            process_name: process.name().to_string_lossy().to_string(),
                            detected_at: chrono::Local::now().to_rfc3339(),
                            is_active_meeting: true, // Teams process usually means active meeting
                        });
                    }
                }
            }

            // Check for Google Meet (browser-based)
            // This requires platform-specific window title detection
            if settings.detect_google_meet {
                if let Some(meeting) = self.detect_google_meet_in_browser(&name, process) {
                    return Some(meeting);
                }
            }
        }

        None
    }

    /// Detect Google Meet running in a browser
    /// This is a simplified check - full implementation requires window title inspection
    #[cfg(target_os = "macos")]
    fn detect_google_meet_in_browser(
        &self,
        process_name: &str,
        _process: &sysinfo::Process,
    ) -> Option<DetectedMeeting> {
        // On macOS, we can use accessibility APIs to check window titles
        // For now, we'll use a simplified approach that checks for browser processes
        // A full implementation would use the Accessibility framework

        for browser in BROWSER_PROCESSES {
            if process_name.contains(&browser.to_lowercase()) {
                // TODO: Implement window title checking via Accessibility API
                // For now, we can't reliably detect Google Meet without window inspection
                // This would require checking if any window title contains "meet.google.com"
                debug!(
                    "Browser detected: {} - Google Meet detection requires window title inspection",
                    browser
                );
            }
        }

        None
    }

    #[cfg(not(target_os = "macos"))]
    fn detect_google_meet_in_browser(
        &self,
        _process_name: &str,
        _process: &sysinfo::Process,
    ) -> Option<DetectedMeeting> {
        // On Windows/Linux, window title detection requires platform-specific APIs
        // Windows: EnumWindows + GetWindowText
        // Linux: X11/Wayland APIs
        None
    }

    /// Start the background monitoring task
    pub async fn start_monitoring<R: Runtime>(&self, app: AppHandle<R>) {
        if self.is_monitoring.load(Ordering::SeqCst) {
            warn!("Meeting detection is already running");
            return;
        }

        self.is_monitoring.store(true, Ordering::SeqCst);
        let my_generation = self.monitor_generation.fetch_add(1, Ordering::SeqCst) + 1;
        info!("Starting meeting detection monitor (generation {})", my_generation);

        let is_monitoring = self.is_monitoring.clone();
        let generation = self.monitor_generation.clone();
        let settings = self.settings.clone();
        let current_meeting = self.current_meeting.clone();
        let auto_recording_active = self.auto_recording_active.clone();

        tokio::spawn(async move {
            let mut system = System::new_with_specifics(
                RefreshKind::new().with_processes(ProcessRefreshKind::new()),
            );
            let mut was_in_meeting = false;
            // Set while the meeting process is gone but we're still waiting out
            // the grace period, in case it's a drop-and-rejoin rather than a
            // real end of meeting.
            let mut pending_stop_since: Option<Instant> = None;
            // True once we've actually seen the auto-started recording running.
            // Needed to tell "user stopped it manually" apart from "the
            // frontend's start checks haven't finished yet".
            let mut auto_recording_observed = false;
            // Deadline for the auto-started recording to actually begin; if it
            // passes without a recording ever being observed, the frontend
            // bailed and we surface that instead of staying silently stuck.
            let mut auto_start_deadline: Option<Instant> = None;

            while is_monitoring.load(Ordering::SeqCst)
                && generation.load(Ordering::SeqCst) == my_generation
            {
                let current_settings = settings.read().await.clone();

                if !current_settings.enabled {
                    tokio::time::sleep(Duration::from_secs(current_settings.poll_interval_secs))
                        .await;
                    continue;
                }

                // A manual stop hands the recording back to the user: once the
                // auto-started recording has been seen running, a later
                // not-recording state means the user stopped it themselves, so
                // auto-stop must not touch whatever they record next.
                if auto_recording_active.load(Ordering::SeqCst) {
                    if crate::is_recording().await {
                        auto_recording_observed = true;
                        auto_start_deadline = None;
                    } else if auto_recording_observed {
                        info!("Recording was stopped manually; releasing auto-stop ownership");
                        auto_recording_active.store(false, Ordering::SeqCst);
                        auto_recording_observed = false;
                    } else if auto_start_deadline.is_some_and(|d| Instant::now() >= d) {
                        // The frontend never started the recording (model not
                        // ready, device error, ...). Its own error UI is in a
                        // hidden window, so reset the tray out of "Starting..."
                        // and tell the user via a real OS notification.
                        warn!(
                            "Auto-started recording did not begin within {}s; giving up",
                            AUTO_START_TIMEOUT.as_secs()
                        );
                        auto_recording_active.store(false, Ordering::SeqCst);
                        auto_start_deadline = None;
                        crate::tray::update_tray_menu_async(&app).await;
                        if let Err(e) = app
                            .notification()
                            .builder()
                            .title("Meetily couldn't start recording")
                            .body("A meeting was detected but recording didn't start. Open Meetily to record it.")
                            .show()
                        {
                            warn!("Failed to show auto-start failure notification: {}", e);
                        }
                    }
                } else {
                    auto_recording_observed = false;
                    auto_start_deadline = None;
                }

                system.refresh_processes(ProcessesToUpdate::All, true);

                // Detect meeting using inline logic (can't call &mut self in spawned task)
                let meeting = detect_meeting_from_system(&system, &current_settings);

                match (was_in_meeting, meeting.is_some()) {
                    (false, true) => {
                        // Meeting started
                        let meeting_info = meeting.unwrap();
                        info!(
                            "Meeting detected: {} ({})",
                            meeting_info.app_name, meeting_info.process_name
                        );

                        // Store current meeting
                        {
                            let mut current = current_meeting.write().await;
                            *current = Some(meeting_info.clone());
                        }

                        // Emit event to frontend
                        let _ = app.emit("meeting-detected", &meeting_info);

                        // Show notification if enabled. This must be a real OS
                        // notification: the window is normally hidden in the
                        // tray, so an in-app event alone would never be seen.
                        if current_settings.notify_on_detection {
                            let body = if current_settings.auto_start_recording {
                                "Meetily is starting a recording"
                            } else {
                                "Open Meetily to record this meeting"
                            };
                            if let Err(e) = app
                                .notification()
                                .builder()
                                .title(format!("{} Meeting Detected", meeting_info.app_name))
                                .body(body)
                                .show()
                            {
                                warn!("Failed to show meeting-detected notification: {}", e);
                            }
                        }

                        // Auto-start recording if enabled
                        if current_settings.auto_start_recording {
                            if crate::is_recording().await {
                                info!("Already recording, skipping auto-start");
                            } else {
                                // Prefer the title of whatever calendar event is
                                // happening right now over a generic app name.
                                // Run it off-thread with a timeout: AppleScript
                                // queries over large calendars can take many
                                // seconds and must not delay the recording. Only
                                // attempt it while Calendar.app is already
                                // running, because `tell application` would
                                // otherwise launch it mid-join.
                                let calendar_running = system_has_process(&system, "Calendar");
                                let fallback_name = format!("{} Meeting", meeting_info.app_name);
                                let meeting_name = match tokio::time::timeout(
                                    Duration::from_secs(3),
                                    tokio::task::spawn_blocking(move || {
                                        current_calendar_event_title(calendar_running)
                                    }),
                                )
                                .await
                                {
                                    Ok(Ok(Some(title))) => title,
                                    _ => fallback_name,
                                };
                                info!("Auto-starting recording for: {}", meeting_name);

                                // Emit event for any UI that wants to react
                                let _ = app.emit(
                                    "auto-start-recording",
                                    serde_json::json!({
                                        "meeting_name": meeting_name,
                                        "app_name": meeting_info.app_name
                                    }),
                                );

                                // Drive the same start path the tray uses, so the frontend
                                // runs its model-readiness and device checks first.
                                trigger_auto_start(&app, &meeting_name);

                                auto_recording_active.store(true, Ordering::SeqCst);
                                auto_recording_observed = false;
                                auto_start_deadline = Some(Instant::now() + AUTO_START_TIMEOUT);
                            }
                        }

                        was_in_meeting = true;
                        pending_stop_since = None;
                    }
                    (true, true) => {
                        // Still in a meeting. If the process had briefly
                        // disappeared (e.g. Zoom dropped and rejoined), cancel
                        // the pending auto-stop so recording continues instead
                        // of being split into two files.
                        if pending_stop_since.take().is_some() {
                            info!("Meeting process reappeared within the grace period, cancelling auto-stop");
                        }

                        // Refresh the reported meeting only if the app changed,
                        // so detected_at keeps meaning "when the meeting was
                        // first seen" rather than "last poll".
                        if let Some(new_meeting) = meeting {
                            let mut current = current_meeting.write().await;
                            match current.as_mut() {
                                Some(cur) if cur.app_name == new_meeting.app_name => {}
                                _ => *current = Some(new_meeting),
                            }
                        }
                    }
                    (true, false) => {
                        // Meeting process disappeared. Don't treat it as ended
                        // right away -- wait out a grace period first, in case
                        // this is a drop-and-rejoin rather than a real end of
                        // meeting.
                        let grace = Duration::from_secs(current_settings.auto_stop_grace_secs);
                        let since = *pending_stop_since.get_or_insert_with(|| {
                            info!(
                                "Meeting process disappeared, waiting up to {}s before treating it as ended",
                                current_settings.auto_stop_grace_secs
                            );
                            Instant::now()
                        });

                        if since.elapsed() >= grace {
                            info!("Meeting ended (grace period elapsed)");
                            pending_stop_since = None;

                            // Clear current meeting
                            {
                                let mut current = current_meeting.write().await;
                                *current = None;
                            }

                            // Emit event to frontend
                            let _ = app.emit("meeting-ended", ());

                            // Auto-stop recording if enabled and we auto-started
                            if current_settings.auto_stop_recording
                                && auto_recording_active.load(Ordering::SeqCst)
                            {
                                info!("Auto-stopping recording");
                                let _ = app.emit("auto-stop-recording", ());

                                // Stop in the backend rather than relying on the
                                // frontend, so this works with the window closed to tray.
                                trigger_auto_stop(&app).await;

                                auto_recording_active.store(false, Ordering::SeqCst);
                            }

                            was_in_meeting = false;
                        }
                        // else: still within the grace period -- keep recording,
                        // keep was_in_meeting = true, and re-check next poll.
                    }
                    (false, false) => {} // No state change
                }

                tokio::time::sleep(Duration::from_secs(current_settings.poll_interval_secs)).await;
            }

            info!("Meeting detection monitor stopped");
        });
    }

    /// Stop the background monitoring task
    pub fn stop_monitoring(&self) {
        info!("Stopping meeting detection monitor");
        // Bump the generation as well as clearing the flag: if monitoring is
        // restarted before the old loop next wakes, the flag alone would read
        // true again and the old loop would keep running alongside the new one.
        self.monitor_generation.fetch_add(1, Ordering::SeqCst);
        self.is_monitoring.store(false, Ordering::SeqCst);
    }
}

impl Default for MeetingDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper function to detect meetings from a System instance
/// Used in the spawned monitoring task
fn detect_meeting_from_system(
    system: &System,
    settings: &MeetingDetectionSettings,
) -> Option<DetectedMeeting> {
    for (_pid, process) in system.processes() {
        let name = process.name().to_string_lossy().to_lowercase();

        // Check for Zoom - only detect ACTIVE meetings via CptHost process
        if settings.detect_zoom {
            // CptHost is the process that runs during an active Zoom meeting on macOS
            if name.contains("cpthost") {
                return Some(DetectedMeeting {
                    app_name: "Zoom".to_string(),
                    process_name: process.name().to_string_lossy().to_string(),
                    detected_at: chrono::Local::now().to_rfc3339(),
                    is_active_meeting: true,
                });
            }
        }

        // Check for Microsoft Teams
        if settings.detect_teams {
            for teams_process in TEAMS_PROCESSES {
                if name.contains(&teams_process.to_lowercase()) {
                    return Some(DetectedMeeting {
                        app_name: "Microsoft Teams".to_string(),
                        process_name: process.name().to_string_lossy().to_string(),
                        detected_at: chrono::Local::now().to_rfc3339(),
                        is_active_meeting: true,
                    });
                }
            }
        }
    }

    None
}

/// True if a process with exactly this name (case-insensitive) is running.
fn system_has_process(system: &System, name: &str) -> bool {
    system
        .processes()
        .values()
        .any(|p| p.name().to_string_lossy().eq_ignore_ascii_case(name))
}

/// Look up the title of whatever calendar event is happening right now, via
/// the macOS Calendar app, so recordings can be named after the meeting
/// instead of a generic "<App> Meeting". Returns `None` if there's no
/// current event, Calendar access hasn't been granted, or `osascript` fails
/// -- callers should fall back to a generic name in that case.
///
/// Blocking (osascript can take seconds on large calendars): callers must run
/// it via `spawn_blocking`, ideally under a timeout. `calendar_running` should
/// come from a process scan -- `tell application "Calendar"` launches the app
/// when it isn't running, and popping Calendar open mid-meeting-join is worse
/// than falling back to a generic recording name.
#[cfg(target_os = "macos")]
fn current_calendar_event_title(calendar_running: bool) -> Option<String> {
    if !calendar_running {
        debug!("Calendar.app is not running, skipping calendar lookup");
        return None;
    }

    // All-day events are excluded: they span the whole day (holidays,
    // birthdays, "Vacation"), so they'd otherwise always win over the
    // actual meeting slot.
    const SCRIPT: &str = r#"
        tell application "Calendar"
            set nowDate to current date
            repeat with cal in calendars
                try
                    set matchingEvents to (every event of cal whose allday event is false and start date ≤ nowDate and end date ≥ nowDate)
                    if (count of matchingEvents) > 0 then
                        return summary of (item 1 of matchingEvents)
                    end if
                end try
            end repeat
            return ""
        end tell
    "#;

    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(SCRIPT)
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if title.is_empty() {
                None
            } else {
                Some(title)
            }
        }
        Ok(output) => {
            debug!(
                "Calendar lookup via osascript failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            None
        }
        Err(e) => {
            debug!("Failed to run osascript for calendar lookup: {}", e);
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn current_calendar_event_title(_calendar_running: bool) -> Option<String> {
    None
}

/// Kick off a recording the same way the tray "Start recording" item does:
/// set the frontend's autoStartRecording flag and route it to the home page,
/// so all model-readiness and audio-device checks still run.
fn trigger_auto_start<R: Runtime>(app: &AppHandle<R>, meeting_name: &str) {
    crate::tray::set_tray_state(app, crate::tray::RecordingState::Starting);

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval("sessionStorage.setItem('autoStartRecording', 'true')");
        // JSON-encode so quotes/apostrophes/unicode in calendar titles can't
        // break out of the string literal.
        let name_json = serde_json::to_string(meeting_name).unwrap_or_else(|_| "null".to_string());
        let _ = window.eval(format!(
            "sessionStorage.setItem('autoStartMeetingTitle', {})",
            name_json
        ));
        let _ = window.eval("window.location.assign('/')");
    } else {
        warn!("No main window available to auto-start recording");
    }
}

/// Stop the active recording from the backend, mirroring the tray stop handler.
/// Emits `recording-stop-complete` so the frontend still does its post-processing
/// (SQLite save, transcription, summary) when it is next visible.
async fn trigger_auto_stop<R: Runtime>(app: &AppHandle<R>) {
    if !crate::is_recording().await {
        info!("Auto-stop requested but nothing is recording");
        // Recompute the tray menu from actual state: if the auto-start never
        // got the recording going, the tray may still be showing "Starting...".
        crate::tray::update_tray_menu_async(app).await;
        return;
    }

    crate::tray::set_tray_state(app, crate::tray::RecordingState::Stopping);

    let data_dir = match app.path().app_data_dir() {
        Ok(dir) => dir,
        Err(e) => {
            error!("Auto-stop: failed to get app data dir: {}", e);
            crate::tray::update_tray_menu_async(app).await;
            return;
        }
    };

    let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let save_path = data_dir.join(format!("recording-{}.wav", timestamp));

    let stop_result = crate::audio::recording_commands::stop_recording(
        app.clone(),
        crate::audio::recording_commands::RecordingArgs {
            save_path: save_path.to_string_lossy().to_string(),
        },
    )
    .await;

    match stop_result {
        Ok(_) => {
            info!("Auto-stop: recording stopped and saved to {:?}", save_path);
            if let Err(e) = app.emit("recording-stop-complete", true) {
                error!("Auto-stop: failed to emit recording-stop-complete: {}", e);
            }
        }
        Err(e) => {
            error!("Auto-stop: failed to stop recording: {}", e);
            crate::tray::update_tray_menu_async(app).await;
        }
    }
}
