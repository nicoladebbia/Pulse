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
/// failed — typically a blocked API). Best-effort; mirrors send_notification's
/// proven-from-launchd delivery path. Distinct from main::notify_failure, which
/// fires only on a hard run-level Err.
pub(crate) fn notify_degraded(msg: &str) {
    // .status() not .spawn() — see notify_failure in main.rs. A fire-and-forget
    // spawn is dropped if the process exits before notificationd delivers.
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"display notification "{}" with title "Pulse fetch DEGRADED" sound name "Basso""#,
            msg.replace('"', "'").replace('\\', "")
        ))
        .status();
}

/// General title+body notification (used by the edge-report money-path hooks).
/// `.status()` not `.spawn()` for the same launchd-delivery reason as above.
pub(crate) fn notify_info(title: &str, body: &str) {
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"display notification "{}" with title "{}" sound name "Glass""#,
            body.replace('"', "'").replace('\\', ""),
            title.replace('"', "'").replace('\\', "")
        ))
        .status();
}
