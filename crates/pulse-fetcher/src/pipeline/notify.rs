pub(crate) fn send_notification(story_count: usize) -> anyhow::Result<()> {
    // .status() not .spawn(): blocks until delivery so the banner survives even
    // if this is the last thing before exit (see notify_failure). Worked before
    // only because post-processing kept the process alive afterward.
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"display notification "Your daily briefing is ready. {} stories across 4 sectors." with title "Pulse" sound name "Glass""#,
            story_count
        ))
        .status()?;
    Ok(())
}

/// Public test hook for `--mode notify-test`: exercises the degraded-alert path.
pub(crate) fn notify_degraded_test(msg: &str) {
    notify_degraded(msg);
}

/// Alert that a run completed but the news briefing is degraded (most summaries
/// failed). The caller now names the OBSERVED cause rather than guessing at a
/// blocked API — see `claude::dominant_failure`. Best-effort; mirrors send_notification's
/// proven-from-launchd delivery path. Distinct from main::notify_failure, which
/// fires only on a hard run-level Err.
pub(crate) fn notify_degraded(msg: &str) {
    // .status() not .spawn() — see notify_failure in main.rs. A fire-and-forget
    // spawn is dropped if the process exits before notificationd delivers.
    run_osascript(
        &format!(
            r#"display notification "{}" with title "Pulse fetch DEGRADED" sound name "Basso""#,
            msg.replace('"', "'").replace('\\', "")
        ),
        "degraded",
    );
}

/// Run one `display notification` script and SAY SO if it did not parse.
///
/// These calls used to discard the exit status entirely. Since the message now
/// embeds a verbatim upstream API body, a body that breaks the AppleScript
/// literal would drop the banner silently — an alarm that fails quietly at the
/// exact moment it is needed, which is this project's most-repeated bug shape.
pub(crate) fn run_osascript(script: &str, which: &str) {
    match std::process::Command::new("osascript").arg("-e").arg(script).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            "osascript rejected the {} notification (status {}): {}",
            which,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => tracing::warn!("could not run osascript for the {} notification: {}", which, e),
    }
}

/// General title+body notification (used by the edge-report money-path hooks).
/// `.status()` not `.spawn()` for the same launchd-delivery reason as above.
pub(crate) fn notify_info(title: &str, body: &str) {
    run_osascript(
        &format!(
            r#"display notification "{}" with title "{}" sound name "Glass""#,
            body.replace('"', "'").replace('\\', ""),
            title.replace('"', "'").replace('\\', "")
        ),
        "info",
    );
}
