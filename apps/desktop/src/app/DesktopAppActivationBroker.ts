// @effect-diagnostics globalTimers:off -- This protocol broker owns cancellable request deadlines outside the Effect runtime.
import {
  DESKTOP_APP_ACTIVATION_PROTOCOL_VERSION,
  type DesktopAppActivationFailure,
  type DesktopAppActivationRequest,
  type DesktopAppActivationResponse,
} from "@onicode/contracts";

interface PendingActivation {
  readonly request: DesktopAppActivationRequest;
  readonly resolve: (response: DesktopAppActivationResponse) => void;
  readonly timeout: ReturnType<typeof setTimeout>;
  /** The page it was sent to, which keeps it across a handler remount. */
  dispatchedTo: RendererPort | null;
}

interface RendererPort {
  readonly send: (request: DesktopAppActivationRequest) => void;
  /** The command got its answer without the page, so the page must not open the folder later. */
  readonly abandon: (requestId: string) => void;
}

function failure(
  requestId: string,
  code: DesktopAppActivationFailure["code"],
  message: string,
): DesktopAppActivationFailure {
  return {
    version: DESKTOP_APP_ACTIVATION_PROTOCOL_VERSION,
    requestId,
    ok: false,
    code,
    message,
  };
}

export class DesktopAppActivationBroker {
  readonly #pending = new Map<string, PendingActivation>();
  readonly #requestTimeoutMs: number;
  readonly #activate: () => void;
  #renderer: RendererPort | null = null;
  #closed = false;

  constructor(input: { readonly requestTimeoutMs: number; readonly activate: () => void }) {
    this.#requestTimeoutMs = input.requestTimeoutMs;
    this.#activate = input.activate;
  }

  request(request: DesktopAppActivationRequest): Promise<DesktopAppActivationResponse> {
    if (this.#closed) {
      return Promise.resolve(
        failure(request.requestId, "renderer-unavailable", "Onicode is shutting down."),
      );
    }
    if (this.#pending.has(request.requestId)) {
      return Promise.resolve(
        failure(request.requestId, "invalid-request", "The request id is already in use."),
      );
    }

    const response = new Promise<DesktopAppActivationResponse>((resolve) => {
      const timeout = setTimeout(() => {
        this.#settle(
          failure(
            request.requestId,
            "request-timeout",
            "The desktop app did not finish opening the project in time.",
          ),
        );
      }, this.#requestTimeoutMs);
      this.#pending.set(request.requestId, { request, resolve, timeout, dispatchedTo: null });
    });

    this.#activate();
    this.#flush();
    return response;
  }

  registerRenderer(renderer: RendererPort): void {
    this.#renderer = renderer;
    this.#flush();
  }

  /** Holds new requests; one already sent can still complete, since its page is still loaded. */
  pauseRenderer(): void {
    this.#renderer = null;
  }

  clearRenderer(): void {
    this.#renderer = null;
    for (const pending of this.#pending.values()) {
      if (pending.dispatchedTo !== null) {
        // Its page is gone, so there is nothing to tell.
        pending.dispatchedTo = null;
        this.#settle(
          failure(
            pending.request.requestId,
            "renderer-unavailable",
            "The Onicode window closed before it opened the project.",
          ),
        );
      }
    }
  }

  complete(response: DesktopAppActivationResponse): void {
    const pending = this.#pending.get(response.requestId);
    if (pending) pending.dispatchedTo = null;
    this.#settle(response);
  }

  cancel(requestId: string): void {
    this.#settle(
      failure(requestId, "renderer-unavailable", "The command closed before Onicode was ready."),
    );
  }

  close(): void {
    this.#closed = true;
    this.#renderer = null;
    for (const pending of this.#pending.values()) {
      this.#settle(
        failure(pending.request.requestId, "renderer-unavailable", "Onicode is shutting down."),
      );
    }
  }

  #flush(): void {
    const renderer = this.#renderer;
    if (renderer === null) return;
    if ([...this.#pending.values()].some((pending) => pending.dispatchedTo !== null)) return;

    for (const pending of this.#pending.values()) {
      try {
        pending.dispatchedTo = renderer;
        renderer.send(pending.request);
      } catch {
        pending.dispatchedTo = null;
        this.#renderer = null;
      }
      return;
    }
  }

  #settle(response: DesktopAppActivationResponse): void {
    const pending = this.#pending.get(response.requestId);
    if (!pending) return;
    clearTimeout(pending.timeout);
    this.#pending.delete(response.requestId);
    try {
      pending.dispatchedTo?.abandon(response.requestId);
    } catch {
      // The page is going away, and it takes its copy of the request with it.
    }
    pending.resolve(response);
    this.#flush();
  }
}
