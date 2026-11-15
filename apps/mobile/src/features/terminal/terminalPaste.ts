import { chunkTerminalWrite } from "@onicode/contracts";

import { encodeTerminalPaste } from "./terminalInput";

interface TerminalPasteInput {
  readonly readText: () => Promise<string>;
  readonly write: (data: string) => Promise<boolean>;
  readonly onReadError: (cause: unknown) => void;
}

export interface TerminalPasteSession {
  readonly reset: (active: boolean) => void;
  /** Sends typed input in order: behind a split write or paste, at once when nothing waits. */
  readonly input: (data: string, write: (chunk: string) => Promise<boolean>) => Promise<boolean>;
  readonly paste: (input: TerminalPasteInput) => Promise<void>;
}

export function createTerminalPasteSession(): TerminalPasteSession {
  let liveTarget: object | null = null;
  let latestRequest = 0;
  let writeTail: Promise<void> = Promise.resolve();
  let queuedWrites = 0;
  const enqueue = <A>(task: () => Promise<A>): Promise<A> => {
    queuedWrites += 1;
    const queued = writeTail.then(task, task);
    const settle = () => {
      queuedWrites -= 1;
    };
    writeTail = queued.then(settle, settle);
    return queued;
  };

  return {
    reset(active) {
      liveTarget = active ? {} : null;
    },

    input(data, write) {
      const target = liveTarget;
      const chunks = chunkTerminalWrite(data);
      const writeChunks = async () => {
        for (const chunk of chunks) {
          if (liveTarget !== target || !(await write(chunk))) return false;
        }
        return true;
      };
      return queuedWrites === 0 && chunks.length === 1 ? writeChunks() : enqueue(writeChunks);
    },

    async paste({ readText, write, onReadError }) {
      const target = liveTarget;
      if (target === null) {
        return;
      }
      const request = ++latestRequest;
      const isCurrent = () => liveTarget === target && latestRequest === request;

      let text: string;
      try {
        text = await readText();
      } catch (cause) {
        onReadError(cause);
        return;
      }

      if (!isCurrent()) {
        return;
      }

      const writePaste = async () => {
        for (const chunk of chunkTerminalWrite(encodeTerminalPaste(text))) {
          if (!isCurrent() || !(await write(chunk))) {
            return;
          }
        }
      };
      await enqueue(writePaste);
    },
  };
}
