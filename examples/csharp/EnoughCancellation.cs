// EnoughCancellation.cs - C# bindings for enough-ffi
//
// This demonstrates how to bridge .NET CancellationToken to Rust's
// cooperative cancellation through FFI.
//
// Safety: The Rust implementation uses Arc-based reference counting,
// making it safe even if the C# handle is disposed while Rust code
// is still using the token. The token keeps the shared state alive.

using System;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;

namespace Enough
{
    /// <summary>
    /// Low-level P/Invoke declarations for the enough-ffi Rust library.
    /// </summary>
    internal static class NativeMethods
    {
        // Update this to match your library name/path
        private const string LibName = "enough_ffi";

        // Source management
        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr enough_cancellation_create();

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void enough_cancellation_cancel(IntPtr source);

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        [return: MarshalAs(UnmanagedType.I1)]
        public static extern bool enough_cancellation_is_cancelled(IntPtr source);

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void enough_cancellation_destroy(IntPtr source);

        // Token management
        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr enough_token_create(IntPtr source);

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr enough_token_create_never();

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        [return: MarshalAs(UnmanagedType.I1)]
        public static extern bool enough_token_is_cancelled(IntPtr token);

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void enough_token_destroy(IntPtr token);
    }

    /// <summary>
    /// A handle to a Rust cancellation source and token pair.
    ///
    /// This class bridges .NET's CancellationToken to Rust's cooperative
    /// cancellation system. When the .NET token is cancelled, the Rust
    /// side sees it immediately.
    ///
    /// Thread-safety: The underlying Rust implementation uses Arc-based
    /// reference counting, so even if Dispose races with Rust code using
    /// the token, no undefined behavior will occur. The shared cancellation
    /// state lives as long as any reference (source or token) exists.
    /// </summary>
    public sealed class CancellationHandle : IDisposable
    {
        private IntPtr _sourcePtr;
        private IntPtr _tokenPtr;
        private CancellationTokenRegistration _registration;
        private volatile bool _disposed;

        /// <summary>
        /// Create a new cancellation handle.
        /// </summary>
        public CancellationHandle()
        {
            _sourcePtr = NativeMethods.enough_cancellation_create();
            if (_sourcePtr == IntPtr.Zero)
            {
                throw new OutOfMemoryException("Failed to create cancellation source");
            }

            _tokenPtr = NativeMethods.enough_token_create(_sourcePtr);
            if (_tokenPtr == IntPtr.Zero)
            {
                NativeMethods.enough_cancellation_destroy(_sourcePtr);
                _sourcePtr = IntPtr.Zero;
                throw new OutOfMemoryException("Failed to create cancellation token");
            }
        }

        /// <summary>
        /// Create a cancellation handle that bridges to a .NET CancellationToken.
        /// When the token is cancelled, the Rust side is notified.
        /// </summary>
        public CancellationHandle(CancellationToken cancellationToken) : this()
        {
            // Register to forward cancellation from .NET to Rust
            // Use useSynchronizationContext: false to avoid deadlocks
            _registration = cancellationToken.Register(CancelCallback, useSynchronizationContext: false);

            // If already cancelled, signal immediately
            if (cancellationToken.IsCancellationRequested)
            {
                NativeMethods.enough_cancellation_cancel(_sourcePtr);
            }
        }

        private void CancelCallback()
        {
            // Safe to call even after Dispose because:
            // 1. We check for IntPtr.Zero
            // 2. Rust handles null gracefully
            // 3. Arc ref counting keeps shared state alive until all refs dropped
            var source = Volatile.Read(ref _sourcePtr);
            if (source != IntPtr.Zero)
            {
                NativeMethods.enough_cancellation_cancel(source);
            }
        }

        /// <summary>
        /// Get the raw token pointer to pass to Rust FFI functions.
        /// </summary>
        /// <remarks>
        /// The token uses Arc-based reference counting in Rust. Even if this
        /// handle is disposed while Rust is using the token, no crash will occur.
        /// However, if disposed without cancellation, the token will never
        /// become cancelled (since no one can call cancel anymore).
        /// </remarks>
        public IntPtr TokenHandle
        {
            get
            {
                ObjectDisposedException.ThrowIf(_disposed, this);
                return _tokenPtr;
            }
        }

        /// <summary>
        /// Check if cancellation has been requested.
        /// </summary>
        public bool IsCancelled
        {
            get
            {
                var token = Volatile.Read(ref _tokenPtr);
                return token != IntPtr.Zero &&
                       NativeMethods.enough_token_is_cancelled(token);
            }
        }

        /// <summary>
        /// Signal cancellation manually.
        /// </summary>
        public void Cancel()
        {
            var source = Volatile.Read(ref _sourcePtr);
            if (source != IntPtr.Zero)
            {
                NativeMethods.enough_cancellation_cancel(source);
            }
        }

        /// <summary>
        /// Dispose of native resources.
        ///
        /// Safe to call even if Rust code is still using the token - the
        /// Arc-based reference counting ensures the shared state stays alive.
        /// </summary>
        public void Dispose()
        {
            if (_disposed) return;
            _disposed = true;

            _registration.Dispose();

            // Destroy token first (it holds a ref to the shared state)
            var token = Interlocked.Exchange(ref _tokenPtr, IntPtr.Zero);
            if (token != IntPtr.Zero)
            {
                NativeMethods.enough_token_destroy(token);
            }

            // Then destroy source
            var source = Interlocked.Exchange(ref _sourcePtr, IntPtr.Zero);
            if (source != IntPtr.Zero)
            {
                NativeMethods.enough_cancellation_destroy(source);
            }

            GC.SuppressFinalize(this);
        }

        /// <summary>
        /// Ensure native resources are freed if Dispose wasn't called.
        /// </summary>
        ~CancellationHandle()
        {
            // Only clean up native resources in finalizer
            var token = Interlocked.Exchange(ref _tokenPtr, IntPtr.Zero);
            if (token != IntPtr.Zero)
            {
                NativeMethods.enough_token_destroy(token);
            }

            var source = Interlocked.Exchange(ref _sourcePtr, IntPtr.Zero);
            if (source != IntPtr.Zero)
            {
                NativeMethods.enough_cancellation_destroy(source);
            }
        }
    }

    /// <summary>
    /// Extension methods for easier integration with async patterns.
    /// </summary>
    public static class CancellationExtensions
    {
        /// <summary>
        /// Run a synchronous Rust operation asynchronously with cancellation support.
        ///
        /// This is the "async-async" pattern: an async C# method that runs
        /// blocking Rust code on a thread pool while respecting cancellation.
        /// </summary>
        /// <typeparam name="T">The result type</typeparam>
        /// <param name="operation">
        /// A function that takes a cancellation token pointer and returns a result.
        /// The Rust code should periodically check the cancellation state.
        /// </param>
        /// <param name="cancellationToken">The .NET cancellation token to bridge</param>
        /// <returns>The result from the Rust operation</returns>
        public static async Task<T> RunWithCancellationAsync<T>(
            Func<IntPtr, T> operation,
            CancellationToken cancellationToken = default)
        {
            using var handle = new CancellationHandle(cancellationToken);

            // Run on thread pool to avoid blocking the async context
            return await Task.Run(() =>
            {
                cancellationToken.ThrowIfCancellationRequested();
                return operation(handle.TokenHandle);
            }, cancellationToken);
        }

        /// <summary>
        /// Run a synchronous Rust operation synchronously with cancellation.
        /// </summary>
        public static T RunWithCancellation<T>(
            Func<IntPtr, T> operation,
            CancellationToken cancellationToken = default)
        {
            using var handle = new CancellationHandle(cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            return operation(handle.TokenHandle);
        }
    }
}
