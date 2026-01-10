// EnoughCancellation.cs - C# bindings for enough-ffi
//
// This demonstrates how to bridge .NET CancellationToken to Rust's
// cooperative cancellation through FFI.

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

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern IntPtr enough_cancellation_create();

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void enough_cancellation_cancel(IntPtr ptr);

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        [return: MarshalAs(UnmanagedType.I1)]
        public static extern bool enough_cancellation_is_cancelled(IntPtr ptr);

        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
        public static extern void enough_cancellation_destroy(IntPtr ptr);
    }

    /// <summary>
    /// A handle to a Rust cancellation source.
    ///
    /// This class bridges .NET's CancellationToken to Rust's cooperative
    /// cancellation system. When the .NET token is cancelled, the Rust
    /// side sees it immediately.
    /// </summary>
    public sealed class CancellationHandle : IDisposable
    {
        private IntPtr _handle;
        private CancellationTokenRegistration _registration;
        private bool _disposed;

        /// <summary>
        /// Create a new cancellation handle.
        /// </summary>
        public CancellationHandle()
        {
            _handle = NativeMethods.enough_cancellation_create();
            if (_handle == IntPtr.Zero)
            {
                throw new OutOfMemoryException("Failed to create cancellation handle");
            }
        }

        /// <summary>
        /// Create a cancellation handle that bridges to a .NET CancellationToken.
        /// When the token is cancelled, the Rust side is notified.
        /// </summary>
        public CancellationHandle(CancellationToken cancellationToken) : this()
        {
            // Register to forward cancellation from .NET to Rust
            _registration = cancellationToken.Register(() =>
            {
                var handle = _handle;
                if (handle != IntPtr.Zero)
                {
                    NativeMethods.enough_cancellation_cancel(handle);
                }
            });

            // If already cancelled, signal immediately
            if (cancellationToken.IsCancellationRequested)
            {
                NativeMethods.enough_cancellation_cancel(_handle);
            }
        }

        /// <summary>
        /// Get the raw pointer to pass to Rust FFI functions.
        /// </summary>
        public IntPtr Handle
        {
            get
            {
                ObjectDisposedException.ThrowIf(_disposed, this);
                return _handle;
            }
        }

        /// <summary>
        /// Check if cancellation has been requested.
        /// </summary>
        public bool IsCancelled
        {
            get
            {
                var handle = _handle;
                return handle != IntPtr.Zero &&
                       NativeMethods.enough_cancellation_is_cancelled(handle);
            }
        }

        /// <summary>
        /// Signal cancellation manually.
        /// </summary>
        public void Cancel()
        {
            var handle = _handle;
            if (handle != IntPtr.Zero)
            {
                NativeMethods.enough_cancellation_cancel(handle);
            }
        }

        public void Dispose()
        {
            if (_disposed) return;
            _disposed = true;

            _registration.Dispose();

            var handle = Interlocked.Exchange(ref _handle, IntPtr.Zero);
            if (handle != IntPtr.Zero)
            {
                NativeMethods.enough_cancellation_destroy(handle);
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
        /// A function that takes a cancellation handle pointer and returns a result.
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
                return operation(handle.Handle);
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
            return operation(handle.Handle);
        }
    }
}
