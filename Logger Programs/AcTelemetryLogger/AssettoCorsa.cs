using System;
using System.Collections.Generic;
using System.IO;
using System.IO.MemoryMappedFiles;
using System.Runtime.InteropServices;
using System.Timers;

namespace AssettoCorsaSharedMemory
{
    public delegate void PhysicsUpdatedHandler(object sender, PhysicsEventArgs e);
    public delegate void GraphicsUpdatedHandler(object sender, GraphicsEventArgs e);
    public delegate void StaticInfoUpdatedHandler(object sender, StaticInfoEventArgs e);
    public delegate void GameStatusChangedHandler(object sender, GameStatusEventArgs e);
    public delegate void PitStatusChangedHandler(object sender, PitStatusEventArgs e);
    public delegate void SessionTypeChangedHandler(object sender, SessionTypeEventArgs e);

    public class AssettoCorsaNotStartedException : Exception
    {
        public AssettoCorsaNotStartedException()
            : base("Shared Memory not connected, is Assetto Corsa running and have you run assettoCorsa.Start()?")
        {
        }
    }

    enum AC_MEMORY_STATUS { DISCONNECTED, CONNECTING, CONNECTED }

    public class AssettoCorsa
    {
        // AC publishes these in the session namespace.
        private const string PhysicsPageName = "Local\\acpmf_physics";
        private const string GraphicsPageName = "Local\\acpmf_graphics";
        private const string StaticPageName = "Local\\acpmf_static";

        private Timer sharedMemoryRetryTimer;
        private AC_MEMORY_STATUS memoryStatus = AC_MEMORY_STATUS.DISCONNECTED;
        public bool IsRunning { get { return (memoryStatus == AC_MEMORY_STATUS.CONNECTED); } }

        private int connectAttempts;
        private string? lastConnectError;

        private long physicsEventsRaised;
        private long graphicsEventsRaised;

        /// <summary>
        /// How many times a physics/graphics event actually fired. Zero means no data
        /// ever arrived, which is a different failure from "data arrived but every frame
        /// was skipped downstream" -- and until these counters existed the two were
        /// indistinguishable in the sidecar.
        /// </summary>
        public long PhysicsEventsRaised => Interlocked.Read(ref physicsEventsRaised);
        public long GraphicsEventsRaised => Interlocked.Read(ref graphicsEventsRaised);

        private AC_STATUS gameStatus = AC_STATUS.AC_OFF;
        private int pitStatus = 1;
        private AC_SESSION_TYPE sessionType = AC_SESSION_TYPE.AC_UNKNOWN;

        public event GameStatusChangedHandler GameStatusChanged;
        public virtual void OnGameStatusChanged(GameStatusEventArgs e)
        {
            if (GameStatusChanged != null)
            {
                GameStatusChanged(this, e);
            }
        }


        public event PitStatusChangedHandler PitStatusChanged;
        public virtual void OnPitStatusChanged(PitStatusEventArgs e)
        {
            if (PitStatusChanged != null)
            {
                PitStatusChanged(this, e);
            }
        }
        public event SessionTypeChangedHandler SessionTypeChanged;
        public virtual void OnSessionTypeChangedHandler(PitStatusEventArgs e)
        {
            if (PitStatusChanged != null)
            {
                PitStatusChanged(this, e);
            }
        }

        public static readonly Dictionary<AC_STATUS, string> StatusNameLookup = new Dictionary<AC_STATUS, string>
        {
            { AC_STATUS.AC_OFF, "Off" },
            { AC_STATUS.AC_LIVE, "Live" },
            { AC_STATUS.AC_PAUSE, "Pause" },
            { AC_STATUS.AC_REPLAY, "Replay" },
        };

        public AssettoCorsa()
        {
            sharedMemoryRetryTimer = new Timer(2000);
            sharedMemoryRetryTimer.AutoReset = true;
            sharedMemoryRetryTimer.Elapsed += sharedMemoryRetryTimer_Elapsed;

            physicsTimer = new Timer();
            physicsTimer.AutoReset = true;
            physicsTimer.Elapsed += physicsTimer_Elapsed;
            PhysicsInterval = 10;

            graphicsTimer = new Timer();
            graphicsTimer.AutoReset = true;
            graphicsTimer.Elapsed += graphicsTimer_Elapsed;
            GraphicsInterval = 1000;

            staticInfoTimer = new Timer();
            staticInfoTimer.AutoReset = true;
            staticInfoTimer.Elapsed += staticInfoTimer_Elapsed;
            StaticInfoInterval = 1000;

            Stop();
        }

        /// <summary>
        /// Connect to the shared memory and start the update timers
        /// </summary>
        public void Start()
        {
            sharedMemoryRetryTimer.Start();
        }

        void sharedMemoryRetryTimer_Elapsed(object sender, ElapsedEventArgs e)
        {
            ConnectToSharedMemory();
        }

        /// <summary>
        /// Attach to the three acpmf_* pages and, only once a read off each has actually
        /// succeeded, start the polling timers.
        ///
        /// This method used to catch FileNotFoundException and nothing else, while being
        /// called from a System.Timers.Timer.Elapsed handler -- and that handler swallows
        /// every exception it sees without a word. So any other failure here vanished:
        /// memoryStatus never reached CONNECTED, the data timers were never started, the
        /// retry timer spun every 2 s forever, and the process looked exactly like it was
        /// patiently "waiting for telemetry". That is why the logger wrote zero frames on
        /// every run, with skipped_no_position also zero -- ProcessPhysics never ran once.
        ///
        /// The failure being swallowed was almost certainly the OpenExisting call below:
        /// the single-argument overload asks for ReadWrite, which AC's pages deny. The
        /// self-test could always read them because it asks for
        /// MemoryMappedFileRights.Read, which is what this now does too.
        /// </summary>
        private bool ConnectToSharedMemory()
        {
            connectAttempts++;
            try
            {
                memoryStatus = AC_MEMORY_STATUS.CONNECTING;

                physicsMMF = MemoryMappedFile.OpenExisting(PhysicsPageName, MemoryMappedFileRights.Read);
                graphicsMMF = MemoryMappedFile.OpenExisting(GraphicsPageName, MemoryMappedFileRights.Read);
                staticInfoMMF = MemoryMappedFile.OpenExisting(StaticPageName, MemoryMappedFileRights.Read);

                // Opening a mapping and mapping a view off it are separate privileges, so
                // three handles are not evidence that a frame can actually be pulled.
                // Prove it before arming any timer -- CONNECTED has to be set first because
                // the Read* methods refuse to run without it.
                memoryStatus = AC_MEMORY_STATUS.CONNECTED;
                ReadStaticInfo();
                ReadGraphics();
                ReadPhysics();

                sharedMemoryRetryTimer.Stop();

                staticInfoTimer.Start();
                ProcessStaticInfo();

                graphicsTimer.Start();
                ProcessGraphics();

                physicsTimer.Start();
                ProcessPhysics();

                Console.WriteLine($"  Shared memory attached (read-only) on attempt {connectAttempts}.");
                lastConnectError = null;
                return true;
            }
            catch (Exception ex)
            {
                memoryStatus = AC_MEMORY_STATUS.DISCONNECTED;
                staticInfoTimer.Stop();
                graphicsTimer.Stop();
                physicsTimer.Stop();
                DisposeHandles();
                ReportConnectFailure(ex);
                return false;
            }
        }

        /// <summary>
        /// Prints why the attach failed, once per distinct error rather than every 2 s.
        /// AC simply not running is expected and gets a quiet line; anything else is the
        /// class of failure that used to be invisible, so it gets a loud one.
        /// </summary>
        private void ReportConnectFailure(Exception ex)
        {
            string key = $"{ex.GetType().Name}: {ex.Message}";
            if (key == lastConnectError) return;
            lastConnectError = key;

            if (ex is FileNotFoundException)
            {
                Console.WriteLine("  Waiting for AC: the acpmf_* pages are not published yet. " +
                                  "Start a session and drive. Retrying every 2 s.");
                return;
            }

            Console.WriteLine();
            Console.WriteLine($"  !! Could not attach to AC shared memory on attempt {connectAttempts}.");
            Console.WriteLine($"  !! {key}");
            if (ex is UnauthorizedAccessException)
            {
                Console.WriteLine("  !! Access denied even read-only. Run this from the same Windows");
                Console.WriteLine("  !! user and session as Assetto Corsa (not elevated-vs-not, not a");
                Console.WriteLine("  !! different remote-desktop session).");
            }
            Console.WriteLine("  !! Retrying every 2 s. Nothing will be logged until this clears.");
            Console.WriteLine();
        }

        private void DisposeHandles()
        {
            try { physicsMMF?.Dispose(); } catch { }
            try { graphicsMMF?.Dispose(); } catch { }
            try { staticInfoMMF?.Dispose(); } catch { }
            physicsMMF = null;
            graphicsMMF = null;
            staticInfoMMF = null;
        }

        /// <summary>
        /// Stop the timers and dispose of the shared memory handles
        /// </summary>
        public void Stop()
        {
            // DISCONNECTED first: it is what makes the Process* methods return early, so
            // an in-flight timer callback cannot be mid-read when the handles go away.
            memoryStatus = AC_MEMORY_STATUS.DISCONNECTED;
            sharedMemoryRetryTimer.Stop();

            // Stop the timers
            physicsTimer.Stop();
            graphicsTimer.Stop();
            staticInfoTimer.Stop();

            DisposeHandles();
        }

        /// <summary>
        /// Interval for physics updates in milliseconds
        /// </summary>
        public double PhysicsInterval
        {
            get
            {
                return physicsTimer.Interval;
            }
            set
            {
                physicsTimer.Interval = value;
            }
        }

        /// <summary>
        /// Interval for graphics updates in milliseconds
        /// </summary>
        public double GraphicsInterval
        {
            get
            {
                return graphicsTimer.Interval;
            }
            set
            {
                graphicsTimer.Interval = value;
            }
        }

        /// <summary>
        /// Interval for static info updates in milliseconds
        /// </summary>
        public double StaticInfoInterval
        {
            get
            {
                return staticInfoTimer.Interval;
            }
            set
            {
                staticInfoTimer.Interval = value;
            }
        }

        MemoryMappedFile physicsMMF;
        MemoryMappedFile graphicsMMF;
        MemoryMappedFile staticInfoMMF;

        Timer physicsTimer;
        Timer graphicsTimer;
        Timer staticInfoTimer;

        /// <summary>
        /// Represents the method that will handle the physics update events
        /// </summary>
        public event PhysicsUpdatedHandler PhysicsUpdated;

        /// <summary>
        /// Represents the method that will handle the graphics update events
        /// </summary>
        public event GraphicsUpdatedHandler GraphicsUpdated;

        /// <summary>
        /// Represents the method that will handle the static info update events
        /// </summary>
        public event StaticInfoUpdatedHandler StaticInfoUpdated;

        public virtual void OnPhysicsUpdated(PhysicsEventArgs e)
        {
            PhysicsUpdated?.Invoke(this, e);
        }

        public virtual void OnGraphicsUpdated(GraphicsEventArgs e)
        {
            if (GraphicsUpdated != null)
            {
                GraphicsUpdated(this, e);
                if (gameStatus != e.Graphics.Status)
                {
                    gameStatus = e.Graphics.Status;
                    GameStatusChanged?.Invoke(this, new GameStatusEventArgs(gameStatus));
                }
                if (pitStatus != e.Graphics.IsInPit)
                {
                    pitStatus = e.Graphics.IsInPit;
                    PitStatusChanged?.Invoke(this, new PitStatusEventArgs(pitStatus));
                }
                if (sessionType != e.Graphics.Session)
                {
                    sessionType = e.Graphics.Session;
                    SessionTypeChanged?.Invoke(this, new SessionTypeEventArgs(sessionType));
                }
            }
        }

        public virtual void OnStaticInfoUpdated(StaticInfoEventArgs e)
        {
            StaticInfoUpdated?.Invoke(this, e);
        }

        private void physicsTimer_Elapsed(object sender, ElapsedEventArgs e)
        {
            ProcessPhysics();
        }

        private void graphicsTimer_Elapsed(object sender, ElapsedEventArgs e)
        {
            ProcessGraphics();
        }

        private void staticInfoTimer_Elapsed(object sender, ElapsedEventArgs e)
        {
            ProcessStaticInfo();
        }

        private readonly HashSet<string> readErrorsReported = new HashSet<string>();

        /// <summary>
        /// The Process* methods below run from Timer.Elapsed handlers, and those swallow
        /// exceptions without a word. A throw escaping one of them would silently stop
        /// that page updating for the rest of the run -- the same class of invisible
        /// failure that hid the connect bug -- so each is caught and reported once per
        /// distinct message.
        ///
        /// They also require CONNECTED rather than merely "not DISCONNECTED". The old
        /// test let CONNECTING through, so a half-attached state could read from handles
        /// that were not all open yet.
        /// </summary>
        private void ReportReadFailure(string page, Exception ex)
        {
            // Both are the ordinary shape of shutdown racing an in-flight callback.
            if (ex is AssettoCorsaNotStartedException || ex is ObjectDisposedException)
                return;

            string key = $"{page}|{ex.GetType().Name}: {ex.Message}";
            lock (readErrorsReported)
            {
                if (!readErrorsReported.Add(key)) return;
            }
            Console.WriteLine($"WARNING: reading {page} failed: {ex.GetType().Name}: {ex.Message}");
        }

        private void ProcessPhysics()
        {
            if (memoryStatus != AC_MEMORY_STATUS.CONNECTED)
                return;

            try
            {
                Physics physics = ReadPhysics();
                Interlocked.Increment(ref physicsEventsRaised);
                OnPhysicsUpdated(new PhysicsEventArgs(physics));
            }
            catch (Exception ex)
            {
                ReportReadFailure("acpmf_physics", ex);
            }
        }

        private void ProcessGraphics()
        {
            if (memoryStatus != AC_MEMORY_STATUS.CONNECTED)
                return;

            try
            {
                Graphics graphics = ReadGraphics();
                Interlocked.Increment(ref graphicsEventsRaised);
                OnGraphicsUpdated(new GraphicsEventArgs(graphics));
            }
            catch (Exception ex)
            {
                ReportReadFailure("acpmf_graphics", ex);
            }
        }

        private void ProcessStaticInfo()
        {
            if (memoryStatus != AC_MEMORY_STATUS.CONNECTED)
                return;

            try
            {
                StaticInfo staticInfo = ReadStaticInfo();
                OnStaticInfoUpdated(new StaticInfoEventArgs(staticInfo));
            }
            catch (Exception ex)
            {
                ReportReadFailure("acpmf_static", ex);
            }
        }

        /// <summary>
        /// Reads one struct out of a shared-memory view.
        ///
        /// The three Read* methods used to do this inline as
        /// <c>reader.ReadBytes(Marshal.SizeOf(...))</c>, which returns *up to* the
        /// requested count without complaining, and then handed the possibly-short
        /// array to PtrToStructure -- which always marshals the full struct size. A
        /// view smaller than the struct therefore read past the end of the managed
        /// array into whatever sat next to it on the heap.
        ///
        /// In practice the OS rounds a view up to a 4096-byte page and all three
        /// structs currently fit inside one, so that path is not reachable today; it
        /// becomes reachable the moment a struct grows past a page. The buffer is now
        /// always full struct size and zero-filled, so a short view yields
        /// deterministic zeros instead of adjacent memory, and shortBy reports it.
        /// </summary>
        /// <param name="shortBy">
        /// Bytes the view could not supply. Always 0 in practice today.
        /// </param>
        private static T ReadStruct<T>(MemoryMappedFile mmf, out int shortBy) where T : struct
        {
            int size = Marshal.SizeOf(typeof(T));
            var buffer = new byte[size];        // zero-filled by the runtime
            int read = 0;

            using (var stream = mmf.CreateViewStream(0, 0, MemoryMappedFileAccess.Read))
            {
                int n;
                while (read < size && (n = stream.Read(buffer, read, size - read)) > 0)
                    read += n;
            }

            shortBy = size - read;

            var handle = GCHandle.Alloc(buffer, GCHandleType.Pinned);
            try
            {
                return Marshal.PtrToStructure<T>(handle.AddrOfPinnedObject());
            }
            finally
            {
                handle.Free();
            }
        }

        private static readonly HashSet<string> _shortReadWarned = new HashSet<string>();

        private static void WarnIfShort(string page, int shortBy)
        {
            if (shortBy <= 0) return;
            lock (_shortReadWarned)
            {
                if (!_shortReadWarned.Add(page)) return;   // once per page, not once per frame
            }
            Console.WriteLine($"WARNING: {page} view is {shortBy} bytes shorter than the struct; " +
                              "the missing tail reads as zeros and those fields are not real data.");
        }

        /// <summary>
        /// Read the current physics data from shared memory
        /// </summary>
        /// <returns>A Physics object representing the current status, or null if not available</returns>
        public Physics ReadPhysics()
        {
            if (memoryStatus != AC_MEMORY_STATUS.CONNECTED || physicsMMF == null)
                throw new AssettoCorsaNotStartedException();

            var data = ReadStruct<Physics>(physicsMMF, out int shortBy);
            WarnIfShort("acpmf_physics", shortBy);
            return data;
        }

        public Graphics ReadGraphics()
        {
            if (memoryStatus != AC_MEMORY_STATUS.CONNECTED || graphicsMMF == null)
                throw new AssettoCorsaNotStartedException();

            var data = ReadStruct<Graphics>(graphicsMMF, out int shortBy);
            WarnIfShort("acpmf_graphics", shortBy);
            return data;
        }

        public StaticInfo ReadStaticInfo()
        {
            if (memoryStatus != AC_MEMORY_STATUS.CONNECTED || staticInfoMMF == null)
                throw new AssettoCorsaNotStartedException();

            var data = ReadStruct<StaticInfo>(staticInfoMMF, out int shortBy);
            WarnIfShort("acpmf_static", shortBy);
            return data;
        }
    }
}
