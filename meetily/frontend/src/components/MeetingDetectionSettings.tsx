import { useEffect, useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Switch } from '@/components/ui/switch';
import { Video, Users, Monitor, Bell, Play, Square, Timer, AlertTriangle } from 'lucide-react';
import { toast } from 'sonner';

interface MeetingDetectionSettings {
  enabled: boolean;
  auto_start_recording: boolean;
  auto_stop_recording: boolean;
  detect_zoom: boolean;
  detect_teams: boolean;
  detect_google_meet: boolean;
  notify_on_detection: boolean;
  poll_interval_secs: number;
  auto_stop_grace_secs: number;
}

interface DetectedMeeting {
  app_name: string;
  process_name: string;
  detected_at: string;
  is_active_meeting: boolean;
}

interface MeetingDetectionStatus {
  is_monitoring: boolean;
  current_meeting: DetectedMeeting | null;
  settings: MeetingDetectionSettings;
  auto_recording_active: boolean;
}

const GRACE_PERIOD_OPTIONS = [15, 30, 60, 120];

export function MeetingDetectionSettings() {
  const [settings, setSettings] = useState<MeetingDetectionSettings | null>(null);
  const [status, setStatus] = useState<MeetingDetectionStatus | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isSaving, setIsSaving] = useState(false);

  useEffect(() => {
    const loadSettings = async () => {
      try {
        const [loadedSettings, loadedStatus] = await Promise.all([
          invoke<MeetingDetectionSettings>('get_meeting_detection_settings'),
          invoke<MeetingDetectionStatus>('get_meeting_detection_status'),
        ]);
        setSettings(loadedSettings);
        setStatus(loadedStatus);
      } catch (error) {
        console.error('Failed to load meeting detection settings:', error);
        toast.error('Failed to load meeting detection settings');
      } finally {
        setIsLoading(false);
      }
    };

    loadSettings();
  }, []);

  useEffect(() => {
    const unsubscribers: (() => void)[] = [];

    const setupListeners = async () => {
      unsubscribers.push(
        await listen<DetectedMeeting>('meeting-detected', (event) => {
          setStatus((prev) => (prev ? { ...prev, current_meeting: event.payload } : prev));
        })
      );

      unsubscribers.push(
        await listen('meeting-ended', () => {
          setStatus((prev) =>
            prev ? { ...prev, current_meeting: null, auto_recording_active: false } : prev
          );
        })
      );
    };

    setupListeners();

    return () => {
      unsubscribers.forEach((unsub) => unsub());
    };
  }, []);

  const updateSettings = useCallback(async (newSettings: MeetingDetectionSettings) => {
    setIsSaving(true);
    try {
      await invoke('set_meeting_detection_settings', { settings: newSettings });
      setSettings(newSettings);

      const newStatus = await invoke<MeetingDetectionStatus>('get_meeting_detection_status');
      setStatus(newStatus);
    } catch (error) {
      console.error('Failed to save meeting detection settings:', error);
      toast.error('Failed to save meeting detection settings');
    } finally {
      setIsSaving(false);
    }
  }, []);

  const handleToggle = (key: keyof MeetingDetectionSettings) => {
    if (!settings) return;
    updateSettings({ ...settings, [key]: !settings[key] });
  };

  const handleGraceSecsChange = (secs: number) => {
    if (!settings) return;
    updateSettings({ ...settings, auto_stop_grace_secs: secs });
  };

  if (isLoading || !settings) {
    return (
      <div className="animate-pulse space-y-4">
        <div className="h-4 bg-gray-200 rounded w-1/4"></div>
        <div className="h-8 bg-gray-200 rounded"></div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-semibold mb-1">Meeting Auto-Detection</h3>
        <p className="text-sm text-gray-600">
          Automatically detect when you join a video meeting and start recording.
        </p>
      </div>

      {/* Master toggle */}
      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1">
          <div className="font-medium">Enable Meeting Detection</div>
          <div className="text-sm text-gray-600">
            Watches for running meeting apps and can auto-start/stop recording
          </div>
        </div>
        <Switch
          checked={settings.enabled}
          onCheckedChange={() => handleToggle('enabled')}
          disabled={isSaving}
        />
      </div>

      {/* Detection is on but nothing will happen automatically -- easy to
          set up by accident, and it looks exactly like a broken feature. */}
      {settings.enabled && !settings.auto_start_recording && (
        <div className="flex items-start gap-3 p-4 rounded-lg border border-amber-200 bg-amber-50">
          <AlertTriangle className="w-5 h-5 text-amber-600 shrink-0 mt-0.5" />
          <div className="flex-1">
            <p className="text-sm font-medium text-amber-900">
              Meetings will be detected, but recording won&apos;t start
            </p>
            <p className="text-sm text-amber-800 mt-0.5">
              Turn on <strong>Auto-start Recording</strong> below for meetings to record
              themselves. Otherwise detection only updates the status shown here.
            </p>
          </div>
          <button
            onClick={() => handleToggle('auto_start_recording')}
            disabled={isSaving}
            className="shrink-0 px-3 py-1.5 text-sm font-medium rounded-md bg-amber-600 text-white hover:bg-amber-700 disabled:opacity-50 transition-colors"
          >
            Turn on
          </button>
        </div>
      )}

      {/* Auto-started recordings would run until stopped by hand. */}
      {settings.enabled && settings.auto_start_recording && !settings.auto_stop_recording && (
        <div className="flex items-start gap-3 p-4 rounded-lg border border-amber-200 bg-amber-50">
          <AlertTriangle className="w-5 h-5 text-amber-600 shrink-0 mt-0.5" />
          <div className="flex-1">
            <p className="text-sm font-medium text-amber-900">
              Recordings won&apos;t stop on their own
            </p>
            <p className="text-sm text-amber-800 mt-0.5">
              With <strong>Auto-stop Recording</strong> off, a recording started by detection
              keeps going after the meeting ends until you stop it manually.
            </p>
          </div>
        </div>
      )}

      {/* Status */}
      {settings.enabled && status && (
        <div
          className={`p-4 rounded-lg border ${
            status.current_meeting ? 'bg-green-50 border-green-200' : 'bg-gray-50 border-gray-200'
          }`}
        >
          <div className="flex items-center space-x-3">
            {status.current_meeting ? (
              <>
                <div className="w-3 h-3 bg-green-500 rounded-full animate-pulse"></div>
                <div>
                  <p className="font-medium text-green-800">
                    {status.current_meeting.app_name} meeting detected
                  </p>
                  <p className="text-sm text-green-600">
                    {status.auto_recording_active ? 'Recording in progress...' : 'Not recording'}
                  </p>
                </div>
              </>
            ) : (
              <>
                <div className="w-3 h-3 bg-gray-400 rounded-full"></div>
                <div>
                  <p className="font-medium text-gray-700">Monitoring for meetings...</p>
                  <p className="text-sm text-gray-500">
                    Checking every {settings.poll_interval_secs}s
                  </p>
                </div>
              </>
            )}
          </div>
        </div>
      )}

      {/* Apps to detect */}
      <div className="space-y-3 pt-2 border-t">
        <h4 className="font-medium text-gray-900 pt-4">Applications to Detect</h4>

        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex items-center space-x-3">
            <Video className="w-5 h-5 text-blue-500" />
            <div>
              <div className="font-medium">Zoom</div>
              <div className="text-sm text-gray-600">
                Detects an active Zoom meeting (native app only, not browser)
              </div>
            </div>
          </div>
          <Switch
            checked={settings.detect_zoom}
            onCheckedChange={() => handleToggle('detect_zoom')}
            disabled={isSaving || !settings.enabled}
          />
        </div>

        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex items-center space-x-3">
            <Users className="w-5 h-5 text-purple-500" />
            <div>
              <div className="font-medium">Microsoft Teams</div>
              <div className="text-sm text-gray-600">
                Detects the Teams process (may false-positive when idle)
              </div>
            </div>
          </div>
          <Switch
            checked={settings.detect_teams}
            onCheckedChange={() => handleToggle('detect_teams')}
            disabled={isSaving || !settings.enabled}
          />
        </div>

        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex items-center space-x-3">
            <Monitor className="w-5 h-5 text-green-500" />
            <div>
              <div className="font-medium">Google Meet</div>
              <div className="text-sm text-gray-600">
                Browser-based detection is not yet implemented
              </div>
            </div>
          </div>
          <Switch
            checked={settings.detect_google_meet}
            onCheckedChange={() => handleToggle('detect_google_meet')}
            disabled={isSaving || !settings.enabled}
          />
        </div>
      </div>

      {/* Recording behavior */}
      <div className="space-y-3 pt-2 border-t">
        <h4 className="font-medium text-gray-900 pt-4">Recording Behavior</h4>

        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex items-center space-x-3">
            <Play className="w-5 h-5 text-red-500" />
            <div>
              <div className="font-medium">Auto-start Recording</div>
              <div className="text-sm text-gray-600">
                Start recording automatically when a meeting is detected
              </div>
            </div>
          </div>
          <Switch
            checked={settings.auto_start_recording}
            onCheckedChange={() => handleToggle('auto_start_recording')}
            disabled={isSaving || !settings.enabled}
          />
        </div>

        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex items-center space-x-3">
            <Square className="w-5 h-5 text-gray-500" />
            <div>
              <div className="font-medium">Auto-stop Recording</div>
              <div className="text-sm text-gray-600">
                Stop recording automatically after the meeting ends
              </div>
            </div>
          </div>
          <Switch
            checked={settings.auto_stop_recording}
            onCheckedChange={() => handleToggle('auto_stop_recording')}
            disabled={isSaving || !settings.enabled}
          />
        </div>

        {settings.auto_stop_recording && (
          <div className="p-4 border rounded-lg bg-gray-50">
            <div className="flex items-center space-x-3 mb-3">
              <Timer className="w-5 h-5 text-orange-500" />
              <div>
                <div className="font-medium">Auto-stop Grace Period</div>
                <div className="text-sm text-gray-600">
                  Wait this long after the meeting app disappears before stopping, so a
                  drop-and-rejoin doesn&apos;t split one meeting into two recordings
                </div>
              </div>
            </div>
            <div className="flex gap-2 pl-8">
              {GRACE_PERIOD_OPTIONS.map((secs) => (
                <button
                  key={secs}
                  onClick={() => handleGraceSecsChange(secs)}
                  disabled={isSaving || !settings.enabled}
                  className={`px-3 py-1.5 text-sm rounded-md border transition-colors disabled:opacity-50 ${
                    settings.auto_stop_grace_secs === secs
                      ? 'bg-blue-600 text-white border-blue-600'
                      : 'bg-white text-gray-700 border-gray-300 hover:bg-gray-50'
                  }`}
                >
                  {secs}s
                </button>
              ))}
            </div>
          </div>
        )}

        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex items-center space-x-3">
            <Bell className="w-5 h-5 text-yellow-500" />
            <div>
              <div className="font-medium">Show Notifications</div>
              <div className="text-sm text-gray-600">
                Show a notification when a meeting is detected
              </div>
            </div>
          </div>
          <Switch
            checked={settings.notify_on_detection}
            onCheckedChange={() => handleToggle('notify_on_detection')}
            disabled={isSaving || !settings.enabled}
          />
        </div>
      </div>

      {/* Privacy notice */}
      <div className="p-4 bg-blue-50 rounded-lg border border-blue-200">
        <p className="text-sm text-blue-800">
          <strong>Privacy note:</strong> meeting detection only looks at running process names to
          identify video conferencing apps. No meeting content, audio, or video is accessed until
          recording actually starts.
        </p>
      </div>
    </div>
  );
}

export default MeetingDetectionSettings;
