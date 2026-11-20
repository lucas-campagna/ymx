/* tslint:disable */
/* eslint-disable */

/**
 * The main YMX WASM library class.
 */
export class Ymx {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Get a callable proxy for a component.
     * Usage: `ymx.greet({ name: "World" })` or `ymx.add(1, 2)`
     * Returns a result object with `.asHtml()` and `.asPdf()` methods.
     */
    __get__(name: string): Function;
    /**
     * Clear all parsed components and reset the project.
     */
    clear(): void;
    components(): Array<any>;
    /**
     * Lint the current project and return findings as a JavaScript object.
     * Returns an object with `findings` array and `diagnostics` array.
     */
    lint(): any;
    constructor();
    parse(code: string): void;
    /**
     * Parse multiple files from a Map<path, content> for _use resolution.
     * Entry point is main.yml (or main.yaml). All .yml files in the map are candidates.
     * _use imports are resolved from main.yml.
     */
    parse_map(file_map: Map<any, any>): void;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_ymx_free: (a: number, b: number) => void;
    readonly ymx___get__: (a: number, b: number, c: number) => [number, number, number];
    readonly ymx_clear: (a: number) => void;
    readonly ymx_components: (a: number) => any;
    readonly ymx_lint: (a: number) => any;
    readonly ymx_new: () => number;
    readonly ymx_parse: (a: number, b: number, c: number) => [number, number];
    readonly ymx_parse_map: (a: number, b: any) => [number, number];
    readonly wasm_bindgen_a0c13ec6d9e5e1bc___convert__closures_____invoke___wasm_bindgen_a0c13ec6d9e5e1bc___JsValue__js_sys_3125a4e75076c85e___Array__wasm_bindgen_a0c13ec6d9e5e1bc___JsValue__true_: (a: number, b: number, c: any, d: any) => any;
    readonly wasm_bindgen_a0c13ec6d9e5e1bc___convert__closures_____invoke___alloc_cb2b8d99256f33df___string__String__true_: (a: number, b: number) => [number, number];
    readonly wasm_bindgen_a0c13ec6d9e5e1bc___convert__closures_____invoke___js_sys_3125a4e75076c85e___Uint8Array__true_: (a: number, b: number) => any;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
