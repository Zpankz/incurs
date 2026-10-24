/* dquickts heap adapter. No native functions are exposed to guest JavaScript. */
#ifdef __EMSCRIPTEN__
#include <emscripten/emscripten.h>
static double now_ms(void) { return emscripten_get_now(); }
#elif defined(_WIN32)
#include <windows.h>
static double now_ms(void) {
    LARGE_INTEGER counter, frequency;
    QueryPerformanceCounter(&counter);
    QueryPerformanceFrequency(&frequency);
    return (double)counter.QuadPart * 1000.0 / (double)frequency.QuadPart;
}
#else
#include <time.h>
static double now_ms(void) {
    struct timespec current;
    clock_gettime(CLOCK_MONOTONIC, &current);
    return (double)current.tv_sec * 1000.0 + (double)current.tv_nsec / 1000000.0;
}
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "quickjs.h"

typedef struct Engine {
    JSRuntime *runtime;
    JSContext *context;
    uint8_t *snapshot;
    size_t snapshot_size;
    char *state;
    char failure[2048];
    double deadline;
    double interval;
    int expired;
    unsigned remaining;
    unsigned interrupt_limit;
} Engine;

#define runtime (engine->runtime)
#define context (engine->context)
#define snapshot (engine->snapshot)
#define snapshot_size (engine->snapshot_size)
#define state (engine->state)
#define failure (engine->failure)
#define deadline (engine->deadline)
#define interval (engine->interval)
#define expired (engine->expired)
#define remaining (engine->remaining)
#define interrupt_limit (engine->interrupt_limit)
static const JSSnapshotHost host = {0};

static int interrupt(JSRuntime *rt, void *opaque) {
    (void)rt;
    Engine *engine = opaque;
    expired = remaining == 0 || now_ms() >= deadline;
    if (remaining) remaining--;
    return expired;
}

static int exception(Engine *engine) {
    JSValue value = JS_GetException(context);
    const char *text = JS_ToCString(context, value);
    snprintf(failure, sizeof(failure), "%s", expired ? "CODE_EXECUTION_TIMEOUT: JavaScript interval exceeded" : text ? text : "JavaScript exception");
    JS_FreeCString(context, text);
    if (!expired && JS_IsError(context, value)) {
        JSValue trace = JS_GetPropertyStr(context, value, "stack");
        if (!JS_IsException(trace) && !JS_IsUndefined(trace)) {
            const char *frames = JS_ToCString(context, trace);
            if (frames) {
                size_t length = strlen(failure);
                snprintf(failure + length, sizeof(failure) - length, "\n%s", frames);
                JS_FreeCString(context, frames);
            }
        }
        JS_FreeValue(context, trace);
    }
    JS_FreeValue(context, value);
    return -1;
}

static int initialize(Engine *engine, size_t memory, size_t stack, double timeout, unsigned budget) {
    runtime = JS_NewRuntime();
    if (!runtime) {
        snprintf(failure, sizeof(failure), "Cannot create JavaScript runtime");
        return -1;
    }
    JS_SetMemoryLimit(runtime, memory);
    JS_SetMaxStackSize(runtime, stack);
    interval = timeout;
    deadline = now_ms() + interval;
    expired = 0;
    remaining = interrupt_limit = budget;
    JS_SetInterruptHandler(runtime, interrupt, engine);
    return 0;
}

Engine *cm_alloc(void) { return calloc(1, sizeof(Engine)); }

int cm_create(Engine *engine, const char *source, size_t memory, size_t stack, double timeout, unsigned budget) {
    if (initialize(engine, memory, stack, timeout, budget)) return -1;
    context = JS_NewContext(runtime);
    if (!context) {
        snprintf(failure, sizeof(failure), "Cannot create JavaScript context");
        return -1;
    }
    JSValue value = JS_Eval(context, source, strlen(source), "codemode.js", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(value)) return exception(engine);
    JS_FreeValue(context, value);
    return 0;
}

int cm_restore(Engine *engine, const uint8_t *bytes, size_t length, size_t memory, size_t stack, double timeout, unsigned budget) {
    if (initialize(engine, memory, stack, timeout, budget)) return -1;
    JSSnapshotResult restored;
    if (JS_ReadSnapshot(runtime, bytes, length, NULL, 0, &host, &restored, failure, sizeof(failure))) return -1;
    if (restored.context_count != 1 || restored.root_count != 0) {
        for (int i = 0; i < restored.root_count; i++) JS_FreeValueRT(runtime, restored.roots[i]);
        for (int i = 0; i < restored.context_count; i++) JS_FreeContext(restored.contexts[i]);
        JS_FreeSnapshotResult(runtime, &restored);
        snprintf(failure, sizeof(failure), "Checkpoint has unexpected contexts or host roots");
        return -1;
    }
    context = restored.contexts[0];
    JS_FreeSnapshotResult(runtime, &restored);
    return 0;
}

int cm_pump(Engine *engine) {
    while (JS_IsJobPending(runtime)) {
        if (now_ms() >= deadline) {
            snprintf(failure, sizeof(failure), "CODE_EXECUTION_TIMEOUT: JavaScript interval exceeded");
            return -1;
        }
        JSContext *job_context;
        if (JS_ExecutePendingJob(runtime, &job_context) < 0) return exception(engine);
    }
    if (expired) {
        snprintf(failure, sizeof(failure), "CODE_EXECUTION_TIMEOUT: JavaScript interval exceeded");
        return -1;
    }
    return 0;
}

const char *cm_state(Engine *engine) {
    const char *source = "JSON.stringify({output:globalThis.__incursDurable.output,failure:globalThis.__incursDurable.failure,pending:globalThis.__incursDurable.pending.length ? globalThis.__incursDurable.pending[0].payload : null})";
    JSValue value = JS_Eval(context, source, strlen(source), "state.js", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(value)) { exception(engine); return NULL; }
    const char *text = JS_ToCString(context, value);
    free(state);
    state = text ? strdup(text) : NULL;
    JS_FreeCString(context, text);
    JS_FreeValue(context, value);
    if (!state) snprintf(failure, sizeof(failure), "Cannot encode JavaScript state");
    return state;
}

int cm_respond(Engine *engine, const char *response) {
    deadline = now_ms() + interval;
    remaining = interrupt_limit;
    const char *source = "response => { const next = globalThis.__incursDurable.pending.shift(); next.resolve(JSON.parse(response)); }";
    JSValue function = JS_Eval(context, source, strlen(source), "response.js", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(function)) return exception(engine);
    JSValue argument = JS_NewString(context, response);
    JSValue value = JS_Call(context, function, JS_UNDEFINED, 1, &argument);
    JS_FreeValue(context, function);
    JS_FreeValue(context, argument);
    if (JS_IsException(value)) return exception(engine);
    JS_FreeValue(context, value);
    return 0;
}

const uint8_t *cm_snapshot(Engine *engine) {
    js_free(context, snapshot);
    snapshot = JS_WriteSnapshot(context, &snapshot_size, &host, NULL, 0, 0);
    if (!snapshot) exception(engine);
    return snapshot;
}

size_t cm_snapshot_size(Engine *engine) { return snapshot_size; }
const char *cm_error(Engine *engine) { return failure; }

void cm_destroy(Engine *engine) {
    if (context) {
        js_free(context, snapshot);
        JS_FreeContext(context);
    }
    if (runtime) JS_FreeRuntime(runtime);
    free(state);
    runtime = NULL;
    context = NULL;
    snapshot = NULL;
    snapshot_size = 0;
    state = NULL;
    free(engine);
}
