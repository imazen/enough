// enough.js - Node.js bindings for enough-ffi
//
// This demonstrates how to use Rust cooperative cancellation from Node.js
// using the ffi-napi library.
//
// Safety: The Rust implementation uses Arc-based reference counting,
// making it safe even if the JS handle is garbage collected while Rust code
// is still using the token. The token keeps the shared state alive.

import ffi from 'ffi-napi';
import ref from 'ref-napi';
import { EventEmitter } from 'events';
import path from 'path';
import { fileURLToPath } from 'url';

// Get library path relative to this file
const __dirname = path.dirname(fileURLToPath(import.meta.url));

// Configure library path - update this for your system
const LIBRARY_PATH = process.env.ENOUGH_LIB_PATH ||
    path.join(__dirname, '../../target/release/libenough_ffi');

// Define types
const voidPtr = ref.refType(ref.types.void);

// Load the native library
let lib;
try {
    lib = ffi.Library(LIBRARY_PATH, {
        // Source management
        'enough_cancellation_create': [voidPtr, []],
        'enough_cancellation_cancel': ['void', [voidPtr]],
        'enough_cancellation_is_cancelled': ['bool', [voidPtr]],
        'enough_cancellation_destroy': ['void', [voidPtr]],
        // Token management
        'enough_token_create': [voidPtr, [voidPtr]],
        'enough_token_create_never': [voidPtr, []],
        'enough_token_is_cancelled': ['bool', [voidPtr]],
        'enough_token_destroy': ['void', [voidPtr]],
    });
} catch (e) {
    console.error('Failed to load enough_ffi library:', e.message);
    console.error('Set ENOUGH_LIB_PATH environment variable to the library location');
    console.error('Build the library first: cargo build --release -p enough-ffi');
    process.exit(1);
}

// FinalizationRegistry for cleanup when handles are garbage collected
// This is a safety net - handles should be explicitly disposed when possible
const cleanupRegistry = new FinalizationRegistry(({ sourcePtr, tokenPtr }) => {
    // Destroy token first, then source (order matters for Arc cleanup)
    if (tokenPtr && !tokenPtr.isNull()) {
        lib.enough_token_destroy(tokenPtr);
    }
    if (sourcePtr && !sourcePtr.isNull()) {
        lib.enough_cancellation_destroy(sourcePtr);
    }
});

/**
 * A handle to a Rust cancellation source and token pair.
 *
 * This class bridges JavaScript's AbortSignal to Rust's cooperative
 * cancellation system. When the AbortSignal is triggered, the Rust
 * side sees it immediately.
 *
 * Thread-safety: The underlying Rust implementation uses Arc-based
 * reference counting, so even if dispose races with Rust code using
 * the token, no undefined behavior will occur.
 */
export class CancellationHandle extends EventEmitter {
    #sourcePtr;
    #tokenPtr;
    #disposed = false;
    #abortController;
    #abortListener;

    /**
     * Create a new cancellation handle.
     * @param {AbortSignal} [signal] - Optional AbortSignal to bridge from
     */
    constructor(signal) {
        super();

        this.#sourcePtr = lib.enough_cancellation_create();
        if (this.#sourcePtr.isNull()) {
            throw new Error('Failed to create cancellation source');
        }

        this.#tokenPtr = lib.enough_token_create(this.#sourcePtr);
        if (this.#tokenPtr.isNull()) {
            lib.enough_cancellation_destroy(this.#sourcePtr);
            throw new Error('Failed to create cancellation token');
        }

        // Register for cleanup if not explicitly disposed
        cleanupRegistry.register(this, {
            sourcePtr: this.#sourcePtr,
            tokenPtr: this.#tokenPtr,
        }, this);

        // Bridge AbortSignal if provided
        if (signal) {
            this.#abortListener = () => {
                this.cancel();
            };
            signal.addEventListener('abort', this.#abortListener);

            // If already aborted, cancel immediately
            if (signal.aborted) {
                this.cancel();
            }
        }
    }

    /**
     * Create a cancellation handle with a timeout.
     * @param {number} timeoutMs - Timeout in milliseconds
     * @returns {CancellationHandle}
     */
    static withTimeout(timeoutMs) {
        const controller = new AbortController();
        const handle = new CancellationHandle(controller.signal);
        handle.#abortController = controller;

        setTimeout(() => {
            if (!handle.#disposed) {
                controller.abort();
            }
        }, timeoutMs);

        return handle;
    }

    /**
     * Get the raw token pointer to pass to Rust FFI functions.
     *
     * The token uses Arc-based reference counting in Rust. Even if this
     * handle is disposed while Rust is using the token, no crash will occur.
     * However, if disposed without cancellation, the token will never
     * become cancelled (since no one can call cancel anymore).
     *
     * @returns {Buffer}
     */
    get handle() {
        if (this.#disposed) {
            throw new Error('CancellationHandle has been disposed');
        }
        return this.#tokenPtr;
    }

    /**
     * Check if cancellation has been requested.
     * @returns {boolean}
     */
    get isCancelled() {
        if (!this.#tokenPtr || this.#tokenPtr.isNull()) return false;
        return lib.enough_token_is_cancelled(this.#tokenPtr);
    }

    /**
     * Signal cancellation.
     */
    cancel() {
        if (!this.#sourcePtr || this.#sourcePtr.isNull()) return;
        lib.enough_cancellation_cancel(this.#sourcePtr);
        this.emit('cancelled');
    }

    /**
     * Dispose of the handle and free Rust resources.
     *
     * Safe to call even if Rust code is still using the token - the
     * Arc-based reference counting ensures the shared state stays alive.
     */
    dispose() {
        if (this.#disposed) return;
        this.#disposed = true;

        // Unregister from FinalizationRegistry since we're cleaning up manually
        cleanupRegistry.unregister(this);

        // Remove abort listener if we have one
        // (AbortSignal doesn't have removeEventListener in all versions)

        // Destroy token first (it holds a ref to the shared state)
        if (this.#tokenPtr && !this.#tokenPtr.isNull()) {
            lib.enough_token_destroy(this.#tokenPtr);
            this.#tokenPtr = ref.NULL;
        }

        // Then destroy source
        if (this.#sourcePtr && !this.#sourcePtr.isNull()) {
            lib.enough_cancellation_destroy(this.#sourcePtr);
            this.#sourcePtr = ref.NULL;
        }
    }

    /**
     * Ensure handle is disposed when using `using` syntax (ES2023+).
     */
    [Symbol.dispose]() {
        this.dispose();
    }
}

/**
 * Run a synchronous operation with cancellation support.
 *
 * @template T
 * @param {function(Buffer): T} operation - Function that takes a cancellation token handle
 * @param {AbortSignal} [signal] - Optional AbortSignal to bridge from
 * @returns {T}
 */
export function runWithCancellation(operation, signal) {
    const handle = new CancellationHandle(signal);
    try {
        if (signal?.aborted) {
            throw new DOMException('Operation was aborted', 'AbortError');
        }
        return operation(handle.handle);
    } finally {
        handle.dispose();
    }
}

/**
 * Run an async operation with cancellation support.
 * This wraps a synchronous FFI call to run on the event loop.
 *
 * @template T
 * @param {function(Buffer): T} operation - Function that takes a cancellation token handle
 * @param {AbortSignal} [signal] - Optional AbortSignal to bridge from
 * @returns {Promise<T>}
 */
export async function runWithCancellationAsync(operation, signal) {
    const handle = new CancellationHandle(signal);

    try {
        if (signal?.aborted) {
            throw new DOMException('Operation was aborted', 'AbortError');
        }

        // Use setImmediate to allow the event loop to process cancellation
        return await new Promise((resolve, reject) => {
            // Check periodically for cancellation
            const checkInterval = setInterval(() => {
                if (handle.isCancelled) {
                    clearInterval(checkInterval);
                    reject(new DOMException('Operation was cancelled', 'AbortError'));
                }
            }, 10);

            // Run operation
            setImmediate(() => {
                try {
                    const result = operation(handle.handle);
                    clearInterval(checkInterval);
                    resolve(result);
                } catch (e) {
                    clearInterval(checkInterval);
                    reject(e);
                }
            });
        });
    } finally {
        handle.dispose();
    }
}

/**
 * Run an operation with a timeout.
 *
 * @template T
 * @param {function(Buffer): T} operation - Function that takes a cancellation token handle
 * @param {number} timeoutMs - Timeout in milliseconds
 * @returns {Promise<T>}
 */
export async function runWithTimeout(operation, timeoutMs) {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), timeoutMs);

    try {
        return await runWithCancellationAsync(operation, controller.signal);
    } finally {
        clearTimeout(timeoutId);
    }
}

// Export the raw library for direct access if needed
export { lib as nativeLib };
