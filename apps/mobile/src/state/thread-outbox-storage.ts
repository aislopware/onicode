import { EnvironmentId, MessageId, ThreadId } from "@onicode/contracts";
import * as Schema from "effect/Schema";

import { writeFileAtomically } from "../lib/atomic-file";
import {
  decodeQueuedThreadMessage,
  encodeQueuedThreadMessage,
  type QueuedThreadMessage,
} from "./thread-outbox-model";

const THREAD_OUTBOX_DIRECTORY = "thread-outbox";

const inFlightWrites = new Set<Promise<void>>();

function trackInFlightWrite(operation: Promise<void>): Promise<void> {
  inFlightWrites.add(operation);
  void operation.catch(() => undefined).finally(() => inFlightWrites.delete(operation));
  return operation;
}

/** Waits for in-flight writes before app-update restart. */
export async function flushThreadOutboxWrites(): Promise<void> {
  while (inFlightWrites.size > 0) {
    await Promise.allSettled(inFlightWrites);
  }
}

export class ThreadOutboxStorageError extends Schema.TaggedError<ThreadOutboxStorageError>()(
  "ThreadOutboxStorageError",
  {
    // `decode-message` is a record that was read but is not a queued message: damaged for good.
    operation: Schema.Literals(["load", "read-message", "decode-message", "write", "remove"]),
    environmentId: Schema.NullOr(EnvironmentId),
    threadId: Schema.NullOr(ThreadId),
    messageId: Schema.NullOr(MessageId),
    fileName: Schema.NullOr(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Thread outbox storage operation ${this.operation} failed for environment ${this.environmentId ?? "unknown"}, thread ${this.threadId ?? "unknown"}, message ${this.messageId ?? "unknown"}, file ${this.fileName ?? "unknown"}.`;
  }
}

export interface ThreadOutboxLoadResult {
  readonly messages: ReadonlyArray<QueuedThreadMessage>;
  readonly errors: ReadonlyArray<ThreadOutboxStorageError>;
}

export interface ThreadOutboxStorage {
  readonly load: () => Promise<ThreadOutboxLoadResult>;
  readonly write: (message: QueuedThreadMessage) => Promise<void>;
  readonly remove: (message: QueuedThreadMessage) => Promise<void>;
  /** Deletes a record that still fails to decode; a record that now reads fine is kept. */
  readonly discardDamaged: (fileName: string) => Promise<void>;
}

function messageFileName(messageId: MessageId): string {
  return `${encodeURIComponent(messageId)}.json`;
}

async function getOutboxDirectory() {
  const { Directory, Paths } = await import("expo-file-system");
  const directory = new Directory(Paths.document, THREAD_OUTBOX_DIRECTORY);
  directory.create({ idempotent: true, intermediates: true });
  return directory;
}

async function getMessageFile(messageId: MessageId) {
  const { File } = await import("expo-file-system");
  return new File(await getOutboxDirectory(), messageFileName(messageId));
}

function decodeRecord(
  text: string,
):
  | { readonly ok: true; readonly message: QueuedThreadMessage }
  | { readonly ok: false; readonly cause: unknown } {
  try {
    return { ok: true, message: decodeQueuedThreadMessage(JSON.parse(text) as unknown) };
  } catch (cause) {
    return { ok: false, cause };
  }
}

function recordError(
  operation: "read-message" | "decode-message" | "remove",
  fileName: string,
  cause: unknown,
): ThreadOutboxStorageError {
  return new ThreadOutboxStorageError({
    operation,
    environmentId: null,
    threadId: null,
    messageId: null,
    fileName,
    cause,
  });
}

export const expoThreadOutboxStorage: ThreadOutboxStorage = {
  load: async () => {
    const messages: QueuedThreadMessage[] = [];
    const errors: ThreadOutboxStorageError[] = [];
    try {
      const { File } = await import("expo-file-system");
      const directory = await getOutboxDirectory();

      for (const entry of directory.list()) {
        if (!(entry instanceof File) || !entry.name.endsWith(".json")) {
          continue;
        }
        let text: string;
        try {
          text = await entry.text();
        } catch (cause) {
          errors.push(recordError("read-message", entry.name, cause));
          continue;
        }
        const decoded = decodeRecord(text);
        if (decoded.ok) messages.push(decoded.message);
        else errors.push(recordError("decode-message", entry.name, decoded.cause));
      }
    } catch (cause) {
      throw new ThreadOutboxStorageError({
        operation: "load",
        environmentId: null,
        threadId: null,
        messageId: null,
        fileName: null,
        cause,
      });
    }
    return { messages, errors };
  },
  write: async (message) => {
    const fileName = messageFileName(message.messageId);
    try {
      await trackInFlightWrite(
        (async () => {
          const file = await getMessageFile(message.messageId);
          await writeFileAtomically(file, JSON.stringify(encodeQueuedThreadMessage(message)));
        })(),
      );
    } catch (cause) {
      throw new ThreadOutboxStorageError({
        operation: "write",
        environmentId: message.environmentId,
        threadId: message.threadId,
        messageId: message.messageId,
        fileName,
        cause,
      });
    }
  },
  remove: async (message) => {
    const fileName = messageFileName(message.messageId);
    try {
      const file = await getMessageFile(message.messageId);
      if (file.exists) {
        file.delete();
      }
    } catch (cause) {
      throw new ThreadOutboxStorageError({
        operation: "remove",
        environmentId: message.environmentId,
        threadId: message.threadId,
        messageId: message.messageId,
        fileName,
        cause,
      });
    }
  },
  discardDamaged: async (fileName) => {
    try {
      const { File } = await import("expo-file-system");
      const file = new File(await getOutboxDirectory(), fileName);
      if (!file.exists || decodeRecord(await file.text()).ok) return;
      file.delete();
    } catch (cause) {
      throw recordError("remove", fileName, cause);
    }
  },
};
