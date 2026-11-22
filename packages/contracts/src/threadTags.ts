import * as Schema from "effect/Schema";
import * as SchemaTransformation from "effect/SchemaTransformation";

import { TrimmedNonEmptyString } from "./baseSchemas.ts";

/** Tags match by exact spelling and case after trimming. */
export const ThreadTag = TrimmedNonEmptyString.check(
  Schema.isMaxLength(64),
  Schema.makeFilter((value) =>
    /[\p{Cc}]/u.test(value) ? "A thread tag must be a single line of text." : true,
  ),
);
export type ThreadTag = typeof ThreadTag.Type;

const isStringArray = Schema.is(Schema.Array(Schema.String));

/**
 * Tags read back from storage or a peer. Lenient so a later tag shape never fails a whole thread.
 * A transform from Unknown, not catchDecoding, which RPC Exit wire codecs drop.
 */
export const StoredThreadTags = Schema.Unknown.pipe(
  Schema.decodeTo(
    Schema.Array(Schema.String),
    SchemaTransformation.transform<ReadonlyArray<string>, unknown>({
      decode: (raw) => (isStringArray(raw) ? raw : []),
      encode: (tags) => tags,
    }),
  ),
);
