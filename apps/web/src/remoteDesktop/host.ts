// @effect-diagnostics globalTimers:off -- Drives browser WebRTC objects from their own events, outside any Effect runtime.
import {
  iceGatheringComplete,
  REMOTE_DESKTOP_CLIPBOARD_LIMIT,
  REMOTE_DESKTOP_CONTROL_CHANNEL,
  REMOTE_DESKTOP_INPUT_CHANNEL,
  type RemoteDesktopHostMessage,
  type RemoteDesktopViewerMessage,
} from "@onicode/client-runtime/remote-desktop/protocol";
import type {
  DesktopBridge,
  RemoteDesktopHostRequest,
  RemoteDesktopInputEvent,
} from "@onicode/contracts";

export interface RemoteDesktopHostSession {
  readonly id: string;
  readonly viewer: string;
  readonly displayId: string;
}

// Enough for a 5K desktop to stay sharp; congestion control spends less whenever the path has less.
const MAX_BITRATE = 24_000_000;
const MAX_FRAME_RATE = 60;
const DISCONNECT_GRACE_MS = 5_000;
const CLIPBOARD_POLL_MS = 500;
// Only macOS hosts take input, so this is its paste shortcut.
const PASTE: ReadonlyArray<RemoteDesktopInputEvent> = [
  { type: "key", code: "MetaLeft", down: true },
  { type: "key", code: "KeyV", down: true },
  { type: "key", code: "KeyV", down: false },
  { type: "key", code: "MetaLeft", down: false },
];
// Hardware encoders and decoders exist for these on every client, in this order of preference.
const H264_PROFILES = ["64", "4d", "42"];

/** The negotiated codecs worth choosing over the browser's default, best first. */
export function preferredVideoCodecs(codecs: ReadonlyArray<RTCRtpCodec>) {
  // A browser only negotiates HEVC when it has a hardware codec for it, and HEVC needs fewer bits
  // than H.264 for the same picture. Main is the profile every such decoder takes, and the default.
  const hevc = codecs.filter((codec) => {
    const profile = /(?:^|;)profile-id=(\d+)/.exec(codec.sdpFmtpLine ?? "")?.[1] ?? "1";
    return codec.mimeType.toLowerCase() === "video/h265" && profile === "1";
  });
  const h264 = codecs.filter(
    (codec) =>
      codec.mimeType.toLowerCase() === "video/h264" &&
      (codec.sdpFmtpLine ?? "").includes("packetization-mode=1"),
  );
  const profile = (codec: RTCRtpCodec) => {
    const id = /profile-level-id=([0-9a-f]{2})/i.exec(codec.sdpFmtpLine ?? "")?.[1]?.toLowerCase();
    const rank = H264_PROFILES.indexOf(id ?? "");
    return rank === -1 ? H264_PROFILES.length : rank;
  };
  return [...hevc, ...h264.toSorted((a, b) => profile(a) - profile(b))];
}

/** How far to shrink the capture so the host encodes no more pixels than the viewer shows. */
export function downscaleFor(
  source: { readonly width: number; readonly height: number },
  viewport: { readonly width: number; readonly height: number },
) {
  if (viewport.width <= 0 || viewport.height <= 0) return 1;
  // The viewer letterboxes, so the tighter axis decides the size on its screen.
  const scale = Math.max(source.width / viewport.width, source.height / viewport.height);
  return Math.max(1, Math.round(scale * 100) / 100);
}

/**
 * Shares this machine's displays with remote viewers. Each display is captured once however
 * many watch it; each viewer gets its own peer connection, and so its own encoder and bitrate.
 */
export function createRemoteDesktopHost(
  bridge: DesktopBridge["remoteDesktop"],
  onSessions: (sessions: ReadonlyArray<RemoteDesktopHostSession>) => void,
) {
  interface Capture {
    readonly stream: Promise<MediaStream>;
    viewers: number;
  }
  interface Session extends RemoteDesktopHostSession {
    readonly connection: RTCPeerConnection;
    disconnectTimer: ReturnType<typeof setTimeout> | null;
    /** Open only for a viewer that may control this machine. */
    clipboard: RTCDataChannel | null;
  }
  const captures = new Map<string, Capture>();
  const sessions = new Map<string, Session>();
  // The main process arms one capture at a time.
  let arming: Promise<unknown> = Promise.resolve();

  let clipboardTimer: ReturnType<typeof setInterval> | null = null;
  const shareClipboard = async () => {
    const text = await bridge.readClipboard().catch(() => null);
    if (text === null) return;
    const message: RemoteDesktopHostMessage = { type: "clipboard", text };
    for (const session of sessions.values()) {
      if (session.clipboard?.readyState === "open") session.clipboard.send(JSON.stringify(message));
    }
  };
  const publish = () => {
    onSessions(
      [...sessions.values()].map(({ id, viewer, displayId }) => ({ id, viewer, displayId })),
    );
    void bridge.setViewers(sessions.size);
    const sharing = [...sessions.values()].some((session) => session.clipboard !== null);
    if (sharing && clipboardTimer === null) {
      // The first look only notes where the clipboard stands; later ones report what changed.
      void bridge.readClipboard().catch(() => null);
      clipboardTimer = setInterval(() => void shareClipboard(), CLIPBOARD_POLL_MS);
    } else if (!sharing && clipboardTimer !== null) {
      clearInterval(clipboardTimer);
      clipboardTimer = null;
    }
  };

  const openCapture = (displayId: string) => {
    const opening = arming.then(async () => {
      if (!(await bridge.armCapture(displayId))) {
        throw new Error("That display is no longer connected to the host.");
      }
      try {
        return await navigator.mediaDevices.getDisplayMedia({
          audio: false,
          video: { frameRate: { ideal: MAX_FRAME_RATE, max: MAX_FRAME_RATE } },
        });
      } catch {
        throw new Error("The host could not capture its screen.");
      } finally {
        await bridge.armCapture(null);
      }
    });
    arming = opening.catch(() => undefined);
    return opening;
  };

  const release = (displayId: string) => {
    const capture = captures.get(displayId);
    if (!capture) return;
    capture.viewers -= 1;
    if (capture.viewers > 0) return;
    captures.delete(displayId);
    void capture.stream.then(
      (stream) => {
        for (const track of stream.getTracks()) track.stop();
      },
      () => undefined,
    );
  };

  const close = (id: string) => {
    const session = sessions.get(id);
    if (!session) return;
    sessions.delete(id);
    if (session.disconnectTimer !== null) clearTimeout(session.disconnectTimer);
    session.connection.close();
    release(session.displayId);
    // Whatever the viewer still held down must not stay down on this machine.
    void bridge.endSession(id);
    publish();
  };

  const acquire = async (displayId: string) => {
    let capture = captures.get(displayId);
    if (!capture) {
      const opened: Capture = { stream: openCapture(displayId), viewers: 0 };
      capture = opened;
      captures.set(displayId, opened);
      void opened.stream.then(
        (stream) => {
          // The OS can end a capture: its own "stop sharing" control, or an unplugged display.
          stream.getVideoTracks()[0]?.addEventListener("ended", () => {
            if (captures.get(displayId) === opened) captures.delete(displayId);
            for (const session of sessions.values()) {
              if (session.displayId === displayId) close(session.id);
            }
          });
        },
        () => {
          if (captures.get(displayId) === opened) captures.delete(displayId);
        },
      );
    }
    capture.viewers += 1;
    try {
      return await capture.stream;
    } catch (error) {
      capture.viewers -= 1;
      throw error;
    }
  };

  const tune = async (sender: RTCRtpSender, downscale: number) => {
    const parameters = sender.getParameters();
    // Text has to stay readable: drop frames before pixels when the path cannot carry both.
    parameters.degradationPreference = "maintain-resolution";
    for (const encoding of parameters.encodings) {
      encoding.maxBitrate = MAX_BITRATE;
      encoding.maxFramerate = MAX_FRAME_RATE;
      encoding.scaleResolutionDownBy = downscale;
      encoding.networkPriority = "high";
    }
    await sender.setParameters(parameters);
  };

  const chooseCodec = async (sender: RTCRtpSender) => {
    for (const codec of preferredVideoCodecs(sender.getParameters().codecs)) {
      const parameters = sender.getParameters();
      for (const encoding of parameters.encodings) {
        (encoding as RTCRtpEncodingParameters & { codec?: RTCRtpCodec }).codec = codec;
      }
      try {
        await sender.setParameters(parameters);
        return;
      } catch {
        // The viewer's variant of this codec is one this machine cannot encode; try the next.
      }
    }
  };

  return {
    /** Answers one viewer's offer. A rejection's message is shown to that viewer. */
    async accept(request: RemoteDesktopHostRequest): Promise<string> {
      const capabilities = await bridge.capabilities();
      if (capabilities.screenRecording === false) {
        throw new Error(
          "The host has not allowed Onicode to record its screen. Open Onicode on the host and turn remote desktop on again to grant it.",
        );
      }
      const id = request.requestId;
      const stream = await acquire(request.displayId);
      const track = stream.getVideoTracks()[0];
      const connection = new RTCPeerConnection({ bundlePolicy: "max-bundle" });
      const session: Session = {
        id,
        viewer: request.viewer,
        displayId: request.displayId,
        connection,
        disconnectTimer: null,
        clipboard: null,
      };
      sessions.set(id, session);
      try {
        if (!track) throw new Error("The host could not capture its screen.");
        track.contentHint = "detail";
        let sender: RTCRtpSender | null = null;
        let downscale = 1;
        connection.addEventListener("datachannel", ({ channel }) => {
          if (channel.label === REMOTE_DESKTOP_INPUT_CHANNEL) {
            channel.addEventListener("close", () => close(id));
            channel.addEventListener("message", (message) => {
              let event: RemoteDesktopInputEvent;
              try {
                event = JSON.parse(String(message.data)) as RemoteDesktopInputEvent;
              } catch {
                return;
              }
              // The main process validates the event and checks the OS still allows input.
              void bridge.input({ sessionId: id, displayId: request.displayId, events: [event] });
            });
          } else if (channel.label === REMOTE_DESKTOP_CONTROL_CHANNEL) {
            if (capabilities.inputControl === true && sessions.get(id) === session) {
              session.clipboard = channel;
              publish();
            }
            channel.addEventListener("message", (message) => {
              let viewport: RemoteDesktopViewerMessage;
              try {
                viewport = JSON.parse(String(message.data)) as RemoteDesktopViewerMessage;
              } catch {
                return;
              }
              if (viewport.type === "clipboard") {
                const { text, paste } = viewport;
                if (typeof text !== "string" || text.length > REMOTE_DESKTOP_CLIPBOARD_LIMIT) {
                  return;
                }
                void bridge
                  .writeClipboard(text)
                  .then(() =>
                    paste === true
                      ? bridge.input({ sessionId: id, displayId: request.displayId, events: PASTE })
                      : undefined,
                  )
                  .catch(() => undefined);
                return;
              }
              const settings = track.getSettings();
              if (viewport.type !== "viewport" || !sender || !settings.width || !settings.height) {
                return;
              }
              const next = downscaleFor(
                { width: settings.width, height: settings.height },
                viewport,
              );
              // Each change restarts the encoder, so ignore the small ones a resize drag produces.
              if (Math.abs(next - downscale) / downscale < 0.1) return;
              downscale = next;
              void tune(sender, downscale).catch(() => undefined);
            });
          }
        });
        connection.addEventListener("connectionstatechange", () => {
          if (session.disconnectTimer !== null) clearTimeout(session.disconnectTimer);
          session.disconnectTimer = null;
          const state = connection.connectionState;
          if (state === "failed" || state === "closed") close(id);
          else if (state === "disconnected") {
            session.disconnectTimer = setTimeout(() => close(id), DISCONNECT_GRACE_MS);
          }
        });
        await connection.setRemoteDescription({ type: "offer", sdp: request.offer });
        sender = connection.addTrack(track, stream);
        await connection.setLocalDescription(await connection.createAnswer());
        await tune(sender, downscale).catch(() => undefined);
        await chooseCodec(sender);
        await iceGatheringComplete(connection);
        const answer = connection.localDescription?.sdp;
        if (!answer) throw new Error("The host could not answer.");
        publish();
        return answer;
      } catch (error) {
        close(id);
        throw error;
      }
    },
    disconnect: close,
    stop() {
      for (const id of sessions.keys()) close(id);
    },
  };
}
