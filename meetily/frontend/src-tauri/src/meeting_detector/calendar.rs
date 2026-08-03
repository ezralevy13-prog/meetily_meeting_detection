//! Calendar lookup for naming recordings after the meeting they belong to.
//!
//! Uses EventKit on macOS, which reads the local calendar store directly.
//! Accounts synced into macOS Calendar (Google, iCloud, Exchange, ...) are all
//! visible through it, so a Google Calendar signed in under System Settings ->
//! Internet Accounts works without any OAuth of our own -- and, unlike the
//! AppleScript approach it replaces, it does not require Calendar.app to be
//! running.

use serde::{Deserialize, Serialize};

/// A calendar the user can choose to draw meeting names from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarInfo {
    /// Stable identifier, persisted in settings.
    pub id: String,
    pub title: String,
    /// Account the calendar belongs to, e.g. "Google" or "iCloud".
    pub source: String,
}

/// Whether the app may read calendar data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarAccess {
    /// The user has not been asked yet.
    NotDetermined,
    Granted,
    Denied,
    /// EventKit is unavailable on this platform.
    Unsupported,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{CalendarAccess, CalendarInfo};
    use block2::RcBlock;
    use log::{debug, warn};
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_event_kit::{EKAuthorizationStatus, EKCalendar, EKEntityType, EKEventStore};
    use objc2_foundation::{NSArray, NSDate, NSError};
    use std::sync::mpsc;
    use std::time::Duration;

    pub fn access_status() -> CalendarAccess {
        // SAFETY: reads a process-wide authorization flag; no arguments to
        // get wrong and no objects returned.
        let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        // Compared rather than matched: EKAuthorizationStatus is a newtype over
        // NSInteger with associated constants, not a Rust enum.
        if status == EKAuthorizationStatus::NotDetermined {
            CalendarAccess::NotDetermined
        } else if status == EKAuthorizationStatus::FullAccess {
            CalendarAccess::Granted
        } else {
            // WriteOnly can create events but not read them, which is what we
            // need, so it counts as denied here.
            CalendarAccess::Denied
        }
    }

    /// Show the system permission prompt and wait for the answer.
    ///
    /// Must be called off the main thread (it blocks): the completion handler
    /// is delivered on an arbitrary queue, so blocking the main thread here
    /// would not deadlock EventKit itself, but would freeze the UI while the
    /// prompt is up.
    pub fn request_access() -> CalendarAccess {
        match access_status() {
            CalendarAccess::NotDetermined => {}
            other => return other,
        }

        let store = unsafe { EKEventStore::new() };
        let (tx, rx) = mpsc::channel::<bool>();

        let handler = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            // The receiver may already have timed out; a failed send is fine.
            let _ = tx.send(granted.as_bool());
        });

        // SAFETY: the block matches the completion signature EventKit declares
        // (BOOL, NSError *), and `handler` outlives this call -- EventKit
        // copies/retains the block before returning.
        unsafe {
            store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&handler));
        }

        // Don't wait forever if the prompt is dismissed by other means.
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(true) => CalendarAccess::Granted,
            Ok(false) => CalendarAccess::Denied,
            Err(_) => {
                warn!("Timed out waiting for the calendar permission prompt");
                access_status()
            }
        }
    }

    pub fn list_calendars() -> Vec<CalendarInfo> {
        if access_status() != CalendarAccess::Granted {
            return Vec::new();
        }

        let store = unsafe { EKEventStore::new() };
        // SAFETY: entity type is a valid EKEntityType; the returned array is
        // retained and only read below.
        let calendars = unsafe { store.calendarsForEntityType(EKEntityType::Event) };

        calendars
            .iter()
            .map(|cal| {
                let source = unsafe { cal.source() }
                    .map(|s| unsafe { s.title() }.to_string())
                    .unwrap_or_default();
                CalendarInfo {
                    id: unsafe { cal.calendarIdentifier() }.to_string(),
                    title: unsafe { cal.title() }.to_string(),
                    source,
                }
            })
            .collect()
    }

    /// Title of an event happening right now, preferring the one that started
    /// most recently (the meeting you just joined, rather than an all-day or
    /// long-running block that merely overlaps now).
    ///
    /// `calendar_ids` restricts the search; empty means every calendar.
    pub fn current_event_title(calendar_ids: &[String]) -> Option<String> {
        if access_status() != CalendarAccess::Granted {
            debug!("Calendar access not granted, skipping calendar lookup");
            return None;
        }

        let store = unsafe { EKEventStore::new() };
        let all = unsafe { store.calendarsForEntityType(EKEntityType::Event) };

        // Narrow to the user's chosen calendars. If none of them still exist
        // (renamed account, deleted calendar), fall back to searching all
        // rather than silently returning nothing forever.
        let selected: Vec<_> = if calendar_ids.is_empty() {
            Vec::new()
        } else {
            all.iter()
                .filter(|cal| {
                    let id = unsafe { cal.calendarIdentifier() }.to_string();
                    calendar_ids.iter().any(|wanted| wanted == &id)
                })
                .collect()
        };
        let filter: Option<Retained<NSArray<EKCalendar>>> = if selected.is_empty() {
            None
        } else {
            Some(NSArray::from_retained_slice(&selected))
        };

        // A window around now: events that started up to 4h ago and have not
        // ended. The predicate matches anything overlapping the range, and we
        // filter precisely below.
        let start = NSDate::dateWithTimeIntervalSinceNow(-4.0 * 3600.0);
        let end = NSDate::now();

        // SAFETY: both dates are valid, and `filter` is either None (all
        // calendars) or an array of calendars from this same store.
        let predicate = unsafe {
            store.predicateForEventsWithStartDate_endDate_calendars(
                &start,
                &end,
                filter.as_deref(),
            )
        };
        let events = unsafe { store.eventsMatchingPredicate(&predicate) };

        let now = NSDate::now().timeIntervalSince1970();
        let mut best: Option<(f64, String)> = None;

        for event in events.iter() {
            // All-day events (holidays, birthdays, "Vacation") span the whole
            // day and would always outrank the actual meeting slot.
            if unsafe { event.isAllDay() } {
                continue;
            }

            let starts = unsafe { event.startDate() }.timeIntervalSince1970();
            let ends = unsafe { event.endDate() }.timeIntervalSince1970();
            if starts > now || ends < now {
                continue;
            }

            let title = unsafe { event.title() }.to_string();
            let title = title.trim().to_string();
            if title.is_empty() {
                continue;
            }

            // Most recently started wins.
            if best.as_ref().is_none_or(|(best_start, _)| starts > *best_start) {
                best = Some((starts, title));
            }
        }

        best.map(|(_, title)| title)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{CalendarAccess, CalendarInfo};

    pub fn access_status() -> CalendarAccess {
        CalendarAccess::Unsupported
    }

    pub fn request_access() -> CalendarAccess {
        CalendarAccess::Unsupported
    }

    pub fn list_calendars() -> Vec<CalendarInfo> {
        Vec::new()
    }

    pub fn current_event_title(_calendar_ids: &[String]) -> Option<String> {
        None
    }
}

pub use imp::{access_status, current_event_title, list_calendars, request_access};
