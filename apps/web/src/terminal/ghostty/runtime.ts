import ghosttyWasmUrl from "./vendor/ghostty-vt.wasm?url";
import ghosttyWritePtyWasmUrl from "./vendor/ghostty-write-pty.wasm?url&no-inline";

type WasmFunction = (...args: Array<number | bigint>) => number;

interface TypeField {
  readonly offset: number;
  readonly size: number;
  /** A primitive (`u8`, `i32`, `pointer`, ...) or the name of another layout. */
  readonly type: string;
}

interface TypeBit {
  readonly lsb: number;
  readonly width: number;
}

interface TypeLayout {
  readonly kind: string;
  readonly size: number;
  readonly align: number;
  readonly fields: Readonly<Record<string, TypeField>>;
  readonly bits?: Readonly<Record<string, TypeBit>>;
}

type TypeLayouts = Readonly<Record<string, TypeLayout>>;

const SIGNED_FIELD_TYPES = new Set(["i8", "i16", "i32", "i64"]);

const textDecoder = new TextDecoder();

// GhosttyTerminalOption values the runtime itself sets.
const TERMINAL_OPT = { userdata: 1, writePty: 2, colorScheme: 8 } as const;

const GHOSTTY_COLOR_SCHEME_LIGHT = 0;
const GHOSTTY_COLOR_SCHEME_DARK = 1;

export interface GhosttyPtyCallbacks {
  readonly write: (data: string) => void;
  /** Answers `CSI ? 996 n`; null leaves the query unanswered. */
  readonly colorScheme: () => "light" | "dark" | null;
}

export class GhosttyRuntime {
  readonly memory: WebAssembly.Memory;
  readonly layouts: TypeLayouts;
  private readonly exports: WebAssembly.Exports;
  private memoryView: DataView;
  private memoryU8: Uint8Array;
  private readonly ptyWriters = new Map<number, GhosttyPtyCallbacks>();
  private nextPtyWriterId = 1;
  private writePtyFunctionIndex = 0;
  private colorSchemeFunctionIndex = 0;

  private constructor(instance: WebAssembly.Instance) {
    this.exports = instance.exports;
    const memory = instance.exports.memory;
    if (!(memory instanceof WebAssembly.Memory)) {
      throw new Error("libghostty-vt did not export WebAssembly memory");
    }
    this.memory = memory;
    this.memoryView = new DataView(memory.buffer);
    this.memoryU8 = new Uint8Array(memory.buffer);
    const jsonPointer = this.call("ghostty_type_json");
    const bytes = new Uint8Array(memory.buffer);
    let end = jsonPointer;
    while (end < bytes.length && bytes[end] !== 0) end += 1;
    this.layouts = (
      JSON.parse(textDecoder.decode(bytes.subarray(jsonPointer, end))) as { types: TypeLayouts }
    ).types;
  }

  static async load(): Promise<GhosttyRuntime> {
    const response = await fetch(ghosttyWasmUrl);
    if (!response.ok) {
      throw new Error(`Unable to load libghostty-vt (${response.status})`);
    }
    let instance: WebAssembly.Instance | undefined;
    const imports = {
      env: {
        log: (pointer: number, length: number) => {
          if (!instance) return;
          const memory = instance.exports.memory;
          if (!(memory instanceof WebAssembly.Memory)) return;
          const message = textDecoder.decode(new Uint8Array(memory.buffer, pointer, length));
          console.debug("[libghostty-vt]", message);
        },
      },
    };
    const result = await WebAssembly.instantiate(await response.arrayBuffer(), imports);
    instance = result.instance;
    const runtime = new GhosttyRuntime(result.instance);
    await runtime.installWritePtyTrampoline();
    return runtime;
  }

  call(name: string, ...args: Array<number | bigint>): number {
    return this.fn(name)(...args);
  }

  fn(name: string): WasmFunction {
    const fn = this.exports[name];
    if (typeof fn !== "function") {
      throw new Error(`libghostty-vt export is unavailable: ${name}`);
    }
    return fn as WasmFunction;
  }

  layout(name: string): TypeLayout {
    const layout = this.layouts[name];
    if (!layout) throw new Error(`libghostty-vt type layout is unavailable: ${name}`);
    return layout;
  }

  alloc(size: number): number {
    const pointer = this.call("ghostty_wasm_alloc", size);
    if (pointer === 0) throw new Error(`libghostty-vt failed to allocate ${size} bytes`);
    new Uint8Array(this.memory.buffer, pointer, size).fill(0);
    return pointer;
  }

  free(pointer: number, size: number): void {
    if (pointer !== 0) this.call("ghostty_wasm_free", pointer, size);
  }

  /** A zeroed `size_t` slot for out-lengths; wasm32 `size_t` is 32 bits. */
  allocUsize(): number {
    return this.alloc(4);
  }

  freeUsize(pointer: number): void {
    this.free(pointer, 4);
  }

  allocOpaque(): number {
    const pointer = this.call("ghostty_wasm_alloc_opaque");
    if (pointer === 0) throw new Error("libghostty-vt failed to allocate an opaque pointer");
    // Zero before *_new so partial-initialization cleanup cannot free a garbage pointer.
    new DataView(this.memory.buffer).setUint32(pointer, 0, true);
    return pointer;
  }

  freeOpaque(pointer: number): void {
    if (pointer !== 0) this.call("ghostty_wasm_free_opaque", pointer);
  }

  readPointer(slot: number): number {
    return this.memoryDataView().getUint32(slot, true);
  }

  attachPtyWriter(terminal: number, callbacks: GhosttyPtyCallbacks): number {
    if (this.writePtyFunctionIndex === 0 || this.colorSchemeFunctionIndex === 0) {
      throw new Error("libghostty-vt PTY callback trampoline is unavailable");
    }
    const id = this.nextPtyWriterId++;
    this.ptyWriters.set(id, callbacks);
    this.call("ghostty_terminal_set", terminal, TERMINAL_OPT.userdata, id);
    this.call("ghostty_terminal_set", terminal, TERMINAL_OPT.writePty, this.writePtyFunctionIndex);
    this.call(
      "ghostty_terminal_set",
      terminal,
      TERMINAL_OPT.colorScheme,
      this.colorSchemeFunctionIndex,
    );
    return id;
  }

  detachPtyWriter(terminal: number, id: number): void {
    this.call("ghostty_terminal_set", terminal, TERMINAL_OPT.colorScheme, 0);
    this.call("ghostty_terminal_set", terminal, TERMINAL_OPT.writePty, 0);
    this.call("ghostty_terminal_set", terminal, TERMINAL_OPT.userdata, 0);
    this.ptyWriters.delete(id);
  }

  view(pointer: number, size?: number): DataView {
    return new DataView(this.memoryDataView().buffer, pointer, size);
  }

  bytes(pointer: number, size: number): Uint8Array {
    return new Uint8Array(this.memoryDataView().buffer, pointer, size);
  }

  memoryDataView(): DataView {
    this.refreshMemoryViews();
    return this.memoryView;
  }

  memoryBytes(): Uint8Array {
    this.refreshMemoryViews();
    return this.memoryU8;
  }

  private refreshMemoryViews(): void {
    // Growing memory detaches the old buffer. Reading `memory.buffer` costs far more than this check.
    if (this.memoryU8.byteLength === 0) {
      this.memoryView = new DataView(this.memory.buffer);
      this.memoryU8 = new Uint8Array(this.memory.buffer);
    }
  }

  setField(pointer: number, structName: string, fieldName: string, value: number): void {
    const field = this.field(structName, fieldName);
    const view = this.memoryDataView();
    const offset = pointer + field.offset;
    if (field.type === "f32") return view.setFloat32(offset, value, true);
    switch (field.size) {
      case 1:
        return view.setUint8(offset, value);
      case 2:
        return view.setUint16(offset, value, true);
      case 4:
        return SIGNED_FIELD_TYPES.has(this.primitiveType(field))
          ? view.setInt32(offset, value, true)
          : view.setUint32(offset, value, true);
      case 8:
        return view.setBigUint64(offset, BigInt(value), true);
      default:
        throw new Error(`Unsupported libghostty-vt field: ${structName}.${fieldName}`);
    }
  }

  readField(pointer: number, structName: string, fieldName: string): number {
    const field = this.field(structName, fieldName);
    const view = this.memoryDataView();
    const offset = pointer + field.offset;
    if (field.type === "f32") return view.getFloat32(offset, true);
    switch (field.size) {
      case 1:
        return view.getUint8(offset);
      case 2:
        return view.getUint16(offset, true);
      case 4:
        return SIGNED_FIELD_TYPES.has(this.primitiveType(field))
          ? view.getInt32(offset, true)
          : view.getUint32(offset, true);
      case 8:
        return Number(view.getBigUint64(offset, true));
      default:
        throw new Error(`Unsupported libghostty-vt field: ${structName}.${fieldName}`);
    }
  }

  /** Bit position and width of a packed flag, e.g. `GhosttyStyleFlags.bold`. */
  bit(structName: string, bitName: string): TypeBit {
    const bit = this.layout(structName).bits?.[bitName];
    if (!bit) throw new Error(`libghostty-vt bit is unavailable: ${structName}.${bitName}`);
    return bit;
  }

  private field(structName: string, fieldName: string): TypeField {
    const field = this.layout(structName).fields[fieldName];
    if (!field) throw new Error(`libghostty-vt field is unavailable: ${structName}.${fieldName}`);
    return field;
  }

  // Enums and aliases name their underlying integer type.
  private primitiveType(field: TypeField): string {
    const named = this.layouts[field.type] as
      | (TypeLayout & { readonly underlying?: string; readonly type?: string })
      | undefined;
    return named?.underlying ?? named?.type ?? field.type;
  }

  private async installWritePtyTrampoline(): Promise<void> {
    const response = await fetch(ghosttyWritePtyWasmUrl);
    if (!response.ok) {
      throw new Error(`Unable to load the libghostty-vt PTY trampoline (${response.status})`);
    }
    const result = await WebAssembly.instantiate(await response.arrayBuffer(), {
      env: {
        oni_write_pty: (_terminal: number, userdata: number, pointer: number, length: number) => {
          const callbacks = this.ptyWriters.get(userdata);
          if (!callbacks || length === 0) return;
          callbacks.write(textDecoder.decode(new Uint8Array(this.memory.buffer, pointer, length)));
        },
        oni_color_scheme: (_terminal: number, userdata: number, outScheme: number) => {
          const scheme = this.ptyWriters.get(userdata)?.colorScheme() ?? null;
          if (scheme === null) return 0;
          this.memoryDataView().setUint32(
            outScheme,
            scheme === "dark" ? GHOSTTY_COLOR_SCHEME_DARK : GHOSTTY_COLOR_SCHEME_LIGHT,
            true,
          );
          return 1;
        },
      },
    });
    const table = this.exports.__indirect_function_table;
    if (!(table instanceof WebAssembly.Table)) {
      throw new Error("libghostty-vt did not expose its callback table");
    }
    const install = (name: string): number => {
      const trampoline = result.instance.exports[name];
      if (typeof trampoline !== "function") {
        throw new Error(`libghostty-vt PTY trampoline is missing ${name}`);
      }
      const index = table.length;
      // WebKit grow(1, fn) stores broken type info and traps on call_indirect; table.set canonicalizes it.
      table.grow(1);
      table.set(index, trampoline);
      return index;
    };
    this.writePtyFunctionIndex = install("ghostty_write_pty");
    this.colorSchemeFunctionIndex = install("ghostty_color_scheme");
  }
}

let runtimePromise: Promise<GhosttyRuntime> | null = null;

export function loadGhosttyRuntime(): Promise<GhosttyRuntime> {
  runtimePromise ??= GhosttyRuntime.load().catch((error) => {
    runtimePromise = null;
    throw error;
  });
  return runtimePromise;
}
