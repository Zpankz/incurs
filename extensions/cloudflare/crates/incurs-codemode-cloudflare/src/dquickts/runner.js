// The engine owns guest state; only serialized requests cross into the host.
export async function runCheckpointProgram(options, createEngine, wasm, dispatch) {
  const identity = `${options.identity}/${options.executionId}`;
  const saved = await dispatch({kind: "load_checkpoint", execution_id: options.executionId});
  if (saved && saved.identity !== identity) {
    throw new Error("Checkpoint identity mismatch; refusing to replay source");
  }
  const module = await createEngine({
    instantiateWasm(imports, receive) {
      const instance = new WebAssembly.Instance(wasm, imports);
      receive(instance);
      return instance.exports;
    },
  });
  const engine = new EngineSession(module);
  try {
    engine.load(options, saved);
    return await drive(engine, options.executionId, identity, dispatch);
  } finally { engine.dispose(); }
}

async function drive(engine, executionId, identity, dispatch) {
  for (;;) {
    const state = engine.state();
    await dispatch({kind: "save_checkpoint", execution_id: executionId,
                    checkpoint: {identity, bytes: engine.checkpoint()}});
    if (state.output !== null) return JSON.parse(state.output);
    if (state.failure !== null) throw new Error(state.failure);
    if (state.pending === null) return new Promise(() => {});
    const response = await dispatch({...state.pending, execution_id: executionId});
    if (response.__codemode_control__ === "pause" || response.kind === "pause") {
      return {result: null, error: null, logs: []};
    }
    engine.respond(response);
  }
}

class EngineSession {
  constructor(module) {
    this.module = module;
    this.handle = module._cm_alloc();
    if (!this.handle) throw new Error("Cannot allocate JavaScript engine");
  }

  check(result) {
    if (result < 0) throw this.failure();
    return result;
  }

  failure() { return new Error(this.module.UTF8ToString(this.module._cm_error(this.handle))); }

  string(value, callback) {
    const pointer = this.module.stringToNewUTF8(value);
    if (!pointer) throw new Error("Cannot allocate engine input");
    try { return callback(pointer); } finally { this.module._free(pointer); }
  }

  load(options, saved) {
    const limits = [options.memoryLimit, options.stackSize, options.timeoutMs, options.interruptBudget];
    if (saved) return this.restore(decode(saved.bytes), limits);
    const source = `globalThis.__incursExecutionId = ${JSON.stringify(options.executionId)};\n${options.source}`;
    this.string(source, pointer => this.check(this.module._cm_create(this.handle, pointer, ...limits)));
  }

  restore(bytes, limits) {
    const pointer = this.module._malloc(bytes.length);
    if (!pointer) throw new Error("Cannot allocate checkpoint input");
    try {
      this.module.HEAPU8.set(bytes, pointer);
      this.check(this.module._cm_restore(this.handle, pointer, bytes.length, ...limits));
    } finally { this.module._free(pointer); }
  }

  state() {
    this.check(this.module._cm_pump(this.handle));
    const pointer = this.module._cm_state(this.handle);
    if (!pointer) throw this.failure();
    return JSON.parse(this.module.UTF8ToString(pointer));
  }

  checkpoint() {
    const pointer = this.module._cm_snapshot(this.handle);
    if (!pointer) throw this.failure();
    return encode(this.module.HEAPU8.subarray(pointer, pointer + this.module._cm_snapshot_size(this.handle)));
  }

  respond(response) {
    this.string(JSON.stringify(response), pointer => this.check(this.module._cm_respond(this.handle, pointer)));
  }

  dispose() { this.module._cm_destroy(this.handle); }
}

function encode(bytes) {
  let text = "";
  for (let offset = 0; offset < bytes.length; offset += 32768) {
    text += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
  }
  return btoa(text);
}

function decode(value) {
  return Uint8Array.from(atob(value), character => character.charCodeAt(0));
}
