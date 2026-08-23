using AssettoCorsaSharedMemory;
using System.Globalization;
using System.IO.Compression;
using System.IO.MemoryMappedFiles;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

class Program
{
    // Shared-memory page names. AC publishes these as Local\ in the session namespace.
    private const string PhysicsPage = "Local\\acpmf_physics";
    private const string GraphicsPage = "Local\\acpmf_graphics";
    private const string StaticPage = "Local\\acpmf_static";

    // ---- output ----
    private static FileStream? _fileStream;
    private static GZipStream? _gzipStream;
    private static Stream? _out;
    private static string _outputPath = "";
    private static bool _gzip = true;

    // ---- latest state ----
    private static AssettoCorsa? _ac;
    private static float[]? _lastPosition;
    private static Graphics? _lastGraphics;
    private static StaticInfo? _lastStaticInfo;
    private static bool _hasValidPosition = false;
    private static bool _hasGraphics = false;
    private static readonly object _lock = new object();

    // ---- counters ----
    private static long _sequence = 0;
    private static long _written = 0;
    private static long _skippedNoPosition = 0;
    private static long _skippedDuplicate = 0;
    private static long _serializeErrors = 0;
    private static long _sanitizedStrings = 0;
    private static long _nonFiniteFloats = 0;
    private static int _lastPacketId = int.MinValue;
    private static bool _dedupeByPacketId = false;

    private static int _sinceFlush = 0;
    private const int FlushEveryLines = 200;

    private static readonly ManualResetEventSlim _exit = new ManualResetEventSlim(false);
    private static int _shutdownStarted = 0;
    private static SelfTestReport? _report;
    private static readonly JsonSerializerOptions _jsonOptions = BuildJsonOptions();

    static int Main(string[] args)
    {
        bool selfTestOnly = false, plain = false, dumpPages = false;
        string? outArg = null;
        int physicsMs = 10, graphicsMs = 10, staticMs = 1000;

        for (int i = 0; i < args.Length; i++)
        {
            string a = args[i];
            switch (a)
            {
                case "--selftest": selfTestOnly = true; break;
                case "--dump-graphics":
                case "--dump-pages": dumpPages = true; break;
                case "--plain": plain = true; break;
                case "-h":
                case "--help": PrintUsage(); return 0;
                case "--physics-ms": physicsMs = ParseIntArg(args, ref i, physicsMs); break;
                case "--graphics-ms": graphicsMs = ParseIntArg(args, ref i, graphicsMs); break;
                case "--static-ms": staticMs = ParseIntArg(args, ref i, staticMs); break;
                case "--out": if (i + 1 < args.Length) outArg = args[++i]; break;
                default:
                    if (a.StartsWith("-"))
                    {
                        Console.WriteLine($"Unknown option: {a}");
                        PrintUsage();
                        return 2;
                    }
                    outArg ??= a;
                    break;
            }
        }

        _gzip = !plain;

        Console.WriteLine("AC telemetry logger");
        Console.WriteLine("===================");

        // Straight to the dump: it is a layout diagnostic, so it deliberately skips the
        // self-test (which interprets the same bytes through the structs being checked)
        // and never opens an output file.
        if (dumpPages) return RunDumpGraphics();

        // The layout self-test always runs. A struct/page mismatch is the difference
        // between a usable file and one full of UTF-16 garbage, and it is silent
        // otherwise: PtrToStructure will happily marshal whatever bytes are there.
        _report = RunSelfTest();
        _report.Print();

        if (selfTestOnly)
        {
            string metaOnly = (outArg ?? "telemetry_ac") + ".selftest.json";
            TryWriteJson(metaOnly, _report.ToMetaObject(null, false));
            Console.WriteLine($"\nSelf-test report written to: {metaOnly}");
            Console.WriteLine("Send that file over if the layout checks did not all pass.");
            return _report.AnyFail ? 1 : 0;
        }

        if (_report.AnyFail)
        {
            Console.WriteLine();
            Console.WriteLine("  !! Self-test FAILED -- see above. Logging will still run, and strings");
            Console.WriteLine("  !! are sanitized so the file stays parseable, but fields at or past the");
            Console.WriteLine("  !! first bad offset are not trustworthy. The .meta.json sidecar records");
            Console.WriteLine("  !! this so the analysis side can treat them as unknown, not as real data.");
            Console.WriteLine();
        }

        try
        {
            OpenOutput(outArg, _report);
            Console.WriteLine($"Logging to : {_outputPath}   [{(_gzip ? "gzip" : "plain")}]");
            Console.WriteLine($"Intervals  : physics {physicsMs} ms, graphics {graphicsMs} ms, static {staticMs} ms");

            _dedupeByPacketId = _report.PacketIdAdvanced;
            Console.WriteLine(_dedupeByPacketId
                ? "Dup filter : ON  (Physics.PacketId advances, so repeats are real repeats)"
                : "Dup filter : OFF (PacketId never advanced during the probe -- keeping every frame)");

            Console.CancelKeyPress += (_, e) => { e.Cancel = true; Shutdown("Ctrl+C"); _exit.Set(); };
            AppDomain.CurrentDomain.ProcessExit += (_, _) => Shutdown("process exit");

            _ac = new AssettoCorsa();
            _ac.PhysicsInterval = physicsMs;
            _ac.GraphicsInterval = graphicsMs;
            _ac.StaticInfoInterval = staticMs;
            _ac.PhysicsUpdated += OnPhysicsUpdated;
            _ac.GraphicsUpdated += OnGraphicsUpdated;
            _ac.StaticInfoUpdated += OnStaticInfoUpdated;
            _ac.Start();

            Console.WriteLine();
            Console.WriteLine("Waiting for telemetry. Drive on track. Ctrl+C or any key stops cleanly.");
            StartKeyWatcher();
            using var progress = StartProgressReporter();
            _exit.Wait();

            Shutdown("stop requested");
            return 0;
        }
        catch (Exception ex)
        {
            Console.WriteLine($"Error: {ex.Message}");
            Shutdown("error: " + ex.Message);
            return 1;
        }
    }

    private static void OnStaticInfoUpdated(object? sender, StaticInfoEventArgs e)
    {
        lock (_lock)
        {
            _lastStaticInfo = e.StaticInfo;
        }
    }

    private static void OnGraphicsUpdated(object? sender, GraphicsEventArgs e)
    {
        lock (_lock)
        {
            _lastGraphics = e.Graphics;
            _hasGraphics = true;

            if (e.Graphics.CarCoordinates != null && e.Graphics.CarCoordinates.Length >= 3)
            {
                float x = e.Graphics.CarCoordinates[0];
                float y = e.Graphics.CarCoordinates[1];
                float z = e.Graphics.CarCoordinates[2];
                if (x != 0 || y != 0 || z != 0)
                {
                    _lastPosition = new float[] { x, y, z };
                    _hasValidPosition = true;
                }
            }
        }
    }

    private const int MaxStringLength = 96;

    /// <summary>
    /// Makes a marshalled fixed-size char buffer safe to put in NDJSON.
    ///
    /// The previous version was <c>s?.Replace("\0", "")</c>. That deletes the NUL
    /// terminator but keeps every byte after it. Those trailing bytes are whatever
    /// happened to be in the page -- and when the struct layout does not match what
    /// AC actually publishes, they are arbitrary memory decoded as UTF-16. That is
    /// exactly how control characters, lone surrogates and mojibake got into the log,
    /// and why a line was occasionally clean: when the unwritten tail happened to be
    /// zeros, the marshaller stopped at the first NUL on its own.
    ///
    /// A fixed-size char buffer is a C string, so it ends at the first NUL. Cut there,
    /// then drop anything not safely printable.
    /// </summary>
    /// <summary>
    /// For the fields AC only ever fills with ASCII: lap times, split times, track and
    /// car identifiers, tyre names, version strings.
    ///
    /// Sanitize alone keeps the file parseable but will still pass through mojibake --
    /// a misaligned read decodes as perfectly legal CJK codepoints, and 32 characters of
    /// those look like data. For a field that is structurally ASCII, a non-ASCII
    /// character proves the bytes were not this field, so return empty: obviously
    /// missing beats plausibly wrong.
    /// </summary>
    private static string SanitizeAscii(string? s)
    {
        string clean = SanitizeCore(s, out bool suspicious);
        foreach (char c in clean)
        {
            if (c > 0x7E)
            {
                suspicious = true;
                clean = "";
                break;
            }
        }
        if (suspicious) Interlocked.Increment(ref _sanitizedStrings);
        return clean;
    }

    private static string Sanitize(string? s)
    {
        string result = SanitizeCore(s, out bool suspicious);
        if (suspicious) Interlocked.Increment(ref _sanitizedStrings);
        return result;
    }

    /// <summary>
    /// The sanitizer proper. <paramref name="suspicious"/> comes back true when the
    /// bytes did not really hold a string -- either characters had to be dropped, or
    /// there was non-NUL content living past the terminator. Callers that are not
    /// logging a frame (self-test, filename building) use this directly so they do
    /// not inflate the corruption counter.
    /// </summary>
    private static string SanitizeCore(string? s, out bool suspicious)
    {
        suspicious = false;
        if (string.IsNullOrEmpty(s)) return "";

        int nul = s.IndexOf('\0');
        string t = nul >= 0 ? s.Substring(0, nul) : s;

        var sb = new StringBuilder(Math.Min(t.Length, MaxStringLength));
        for (int i = 0; i < t.Length && sb.Length < MaxStringLength; i++)
        {
            char c = t[i];

            // Unpaired surrogates are the signature of non-text bytes read as UTF-16.
            // They cannot be encoded as UTF-8 and would corrupt or throw downstream.
            if (char.IsHighSurrogate(c))
            {
                if (i + 1 < t.Length && char.IsLowSurrogate(t[i + 1]))
                {
                    sb.Append(c);
                    sb.Append(t[++i]);
                }
                continue;
            }
            if (char.IsLowSurrogate(c)) continue;

            if (c < 0x20 || c == 0x7F) continue;               // C0 controls + DEL
            if (c >= 0x80 && c <= 0x9F) continue;              // C1 controls
            if (c >= 0xFFFE || c == 0xFFFD) continue;          // noncharacters + U+FFFD
            sb.Append(c);
        }

        suspicious = sb.Length != t.Length;
        if (!suspicious && nul >= 0)
        {
            for (int i = nul + 1; i < s.Length; i++)
            {
                if (s[i] != '\0') { suspicious = true; break; }
            }
        }

        return sb.ToString().Trim();
    }

    private static void OnPhysicsUpdated(object? sender, PhysicsEventArgs e)
    {
        // World position only exists on the graphics page, so there is nothing worth
        // writing until that has arrived at least once.
        if (!_hasValidPosition || _lastPosition == null || !_hasGraphics || _lastGraphics == null)
        {
            Interlocked.Increment(ref _skippedNoPosition);
            return;
        }

        lock (_lock)
        {
            if (_out == null) return;

            // AC republishes the same physics frame when we poll faster than it
            // updates. Only trust PacketId for this if the probe saw it advancing --
            // on some builds it stays 0 forever, and deduping on that drops the lot.
            if (_dedupeByPacketId)
            {
                if (e.Physics.PacketId == _lastPacketId) { _skippedDuplicate++; return; }
                _lastPacketId = e.Physics.PacketId;
            }

            try
            {
                var p = e.Physics;
                var g = _lastGraphics.Value;
                var s = _lastStaticInfo;

                // Build the data object with all fields
                var data = new
                {
                    // ---- Timestamp & Position ----
                    Timestamp = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
                    SequenceNumber = ++_sequence,
                    PositionX = _lastPosition[0],
                    PositionY = _lastPosition[1],
                    PositionZ = _lastPosition[2],

                    // ---- Physics (all fields) ----
                    Physics_PacketId = p.PacketId,
                    Physics_Gas = p.Gas,
                    Physics_Brake = p.Brake,
                    Physics_Fuel = p.Fuel,
                    Physics_Gear = p.Gear,
                    Physics_Rpms = p.Rpms,
                    Physics_SteerAngle = p.SteerAngle,
                    Physics_SpeedKmh = p.SpeedKmh,
                    Physics_Velocity0 = p.Velocity?[0] ?? 0,
                    Physics_Velocity1 = p.Velocity?[1] ?? 0,
                    Physics_Velocity2 = p.Velocity?[2] ?? 0,
                    Physics_AccG0 = p.AccG?[0] ?? 0,
                    Physics_AccG1 = p.AccG?[1] ?? 0,
                    Physics_AccG2 = p.AccG?[2] ?? 0,
                    Physics_WheelSlip0 = p.WheelSlip?[0] ?? 0,
                    Physics_WheelSlip1 = p.WheelSlip?[1] ?? 0,
                    Physics_WheelSlip2 = p.WheelSlip?[2] ?? 0,
                    Physics_WheelSlip3 = p.WheelSlip?[3] ?? 0,
                    Physics_WheelLoad0 = p.WheelLoad?[0] ?? 0,
                    Physics_WheelLoad1 = p.WheelLoad?[1] ?? 0,
                    Physics_WheelLoad2 = p.WheelLoad?[2] ?? 0,
                    Physics_WheelLoad3 = p.WheelLoad?[3] ?? 0,
                    Physics_WheelPressure0 = p.WheelPressure?[0] ?? 0,
                    Physics_WheelPressure1 = p.WheelPressure?[1] ?? 0,
                    Physics_WheelPressure2 = p.WheelPressure?[2] ?? 0,
                    Physics_WheelPressure3 = p.WheelPressure?[3] ?? 0,
                    Physics_WheelAngularSpeed0 = p.WheelAngularSpeed?[0] ?? 0,
                    Physics_WheelAngularSpeed1 = p.WheelAngularSpeed?[1] ?? 0,
                    Physics_WheelAngularSpeed2 = p.WheelAngularSpeed?[2] ?? 0,
                    Physics_WheelAngularSpeed3 = p.WheelAngularSpeed?[3] ?? 0,
                    Physics_TyreWear0 = p.TyreWear?[0] ?? 0,
                    Physics_TyreWear1 = p.TyreWear?[1] ?? 0,
                    Physics_TyreWear2 = p.TyreWear?[2] ?? 0,
                    Physics_TyreWear3 = p.TyreWear?[3] ?? 0,
                    Physics_TyreDirtyLevel0 = p.TyreDirtyLevel?[0] ?? 0,
                    Physics_TyreDirtyLevel1 = p.TyreDirtyLevel?[1] ?? 0,
                    Physics_TyreDirtyLevel2 = p.TyreDirtyLevel?[2] ?? 0,
                    Physics_TyreDirtyLevel3 = p.TyreDirtyLevel?[3] ?? 0,
                    Physics_TyreCoreTemp0 = p.TyreCoreTemp?[0] ?? 0,
                    Physics_TyreCoreTemp1 = p.TyreCoreTemp?[1] ?? 0,
                    Physics_TyreCoreTemp2 = p.TyreCoreTemp?[2] ?? 0,
                    Physics_TyreCoreTemp3 = p.TyreCoreTemp?[3] ?? 0,
                    Physics_CamberRad0 = p.CamberRad?[0] ?? 0,
                    Physics_CamberRad1 = p.CamberRad?[1] ?? 0,
                    Physics_CamberRad2 = p.CamberRad?[2] ?? 0,
                    Physics_CamberRad3 = p.CamberRad?[3] ?? 0,
                    Physics_SuspensionTravel0 = p.SuspensionTravel?[0] ?? 0,
                    Physics_SuspensionTravel1 = p.SuspensionTravel?[1] ?? 0,
                    Physics_SuspensionTravel2 = p.SuspensionTravel?[2] ?? 0,
                    Physics_SuspensionTravel3 = p.SuspensionTravel?[3] ?? 0,
                    Physics_Drs = p.Drs,
                    Physics_TC = p.TC,
                    Physics_Heading = p.Heading,
                    Physics_Pitch = p.Pitch,
                    Physics_Roll = p.Roll,
                    Physics_CgHeight = p.CgHeight,
                    Physics_CarDamage0 = p.CarDamage?[0] ?? 0,
                    Physics_CarDamage1 = p.CarDamage?[1] ?? 0,
                    Physics_CarDamage2 = p.CarDamage?[2] ?? 0,
                    Physics_CarDamage3 = p.CarDamage?[3] ?? 0,
                    Physics_CarDamage4 = p.CarDamage?[4] ?? 0,
                    Physics_NumberOfTyresOut = p.NumberOfTyresOut,
                    Physics_PitLimiterOn = p.PitLimiterOn,
                    Physics_Abs = p.Abs,
                    Physics_KersCharge = p.KersCharge,
                    Physics_KersInput = p.KersInput,
                    Physics_AutoShifterOn = p.AutoShifterOn,
                    Physics_RideHeight0 = p.RideHeight?[0] ?? 0,
                    Physics_RideHeight1 = p.RideHeight?[1] ?? 0,
                    Physics_TurboBoost = p.TurboBoost,
                    Physics_Ballast = p.Ballast,
                    Physics_AirDensity = p.AirDensity,
                    Physics_AirTemp = p.AirTemp,
                    Physics_RoadTemp = p.RoadTemp,
                    Physics_LocalAngularVelocity0 = p.LocalAngularVelocity?[0] ?? 0,
                    Physics_LocalAngularVelocity1 = p.LocalAngularVelocity?[1] ?? 0,
                    Physics_LocalAngularVelocity2 = p.LocalAngularVelocity?[2] ?? 0,
                    Physics_FinalFF = p.FinalFF,
                    Physics_PerformanceMeter = p.PerformanceMeter,
                    Physics_EngineBrake = p.EngineBrake,
                    Physics_ErsRecoveryLevel = p.ErsRecoveryLevel,
                    Physics_ErsPowerLevel = p.ErsPowerLevel,
                    Physics_ErsHeatCharging = p.ErsHeatCharging,
                    Physics_ErsisCharging = p.ErsisCharging,
                    Physics_KersCurrentKJ = p.KersCurrentKJ,
                    Physics_DrsAvailable = p.DrsAvailable,
                    Physics_DrsEnabled = p.DrsEnabled,
                    Physics_BrakeTemp0 = p.BrakeTemp?[0] ?? 0,
                    Physics_BrakeTemp1 = p.BrakeTemp?[1] ?? 0,
                    Physics_BrakeTemp2 = p.BrakeTemp?[2] ?? 0,
                    Physics_BrakeTemp3 = p.BrakeTemp?[3] ?? 0,
                    Physics_Clutch = p.Clutch,
                    Physics_TyreTempI0 = p.TyreTempI?[0] ?? 0,
                    Physics_TyreTempI1 = p.TyreTempI?[1] ?? 0,
                    Physics_TyreTempI2 = p.TyreTempI?[2] ?? 0,
                    Physics_TyreTempI3 = p.TyreTempI?[3] ?? 0,
                    Physics_TyreTempM0 = p.TyreTempM?[0] ?? 0,
                    Physics_TyreTempM1 = p.TyreTempM?[1] ?? 0,
                    Physics_TyreTempM2 = p.TyreTempM?[2] ?? 0,
                    Physics_TyreTempM3 = p.TyreTempM?[3] ?? 0,
                    Physics_TyreTempO0 = p.TyreTempO?[0] ?? 0,
                    Physics_TyreTempO1 = p.TyreTempO?[1] ?? 0,
                    Physics_TyreTempO2 = p.TyreTempO?[2] ?? 0,
                    Physics_TyreTempO3 = p.TyreTempO?[3] ?? 0,
                    Physics_IsAIControlled = p.IsAIControlled,
                    Physics_BrakeBias = p.BrakeBias,
                    Physics_LocalVelocity0 = p.LocalVelocity?[0] ?? 0,
                    Physics_LocalVelocity1 = p.LocalVelocity?[1] ?? 0,
                    Physics_LocalVelocity2 = p.LocalVelocity?[2] ?? 0,
                    // localVelocity is the last thing AC's physics page holds (it ends at
                    // 580 bytes). The ACC tail that used to be logged here -- P2PActivation,
                    // P2PStatus, CurrentMaxRpm, TCInAction, ABSInAction, TyreTemp0..3,
                    // WaterTemp, IgnitionOn, StarterEngineOn, IsEngineRunning,
                    // KerbVibration, SlipVibrations, GBibrations, ABSVibrations -- was
                    // reading never-written page and writing it out as zeros that looked
                    // exactly like real readings.

                    // ---- Graphics (all fields, with sanitized strings) ----
                    Graphics_PacketId = g.PacketId,
                    Graphics_Status = (int)g.Status,
                    Graphics_Session = (int)g.Session,
                    Graphics_CurrentTime = SanitizeAscii(g.CurrentTime),
                    Graphics_LastTime = SanitizeAscii(g.LastTime),
                    Graphics_BestTime = SanitizeAscii(g.BestTime),
                    Graphics_Split = SanitizeAscii(g.Split),
                    Graphics_CompletedLaps = g.CompletedLaps,
                    Graphics_Position = g.Position,
                    Graphics_iCurrentTime = g.iCurrentTime,
                    Graphics_iLastTime = g.iLastTime,
                    Graphics_iBestTime = g.iBestTime,
                    Graphics_SessionTimeLeft = g.SessionTimeLeft,
                    Graphics_DistanceTraveled = g.DistanceTraveled,
                    Graphics_IsInPit = g.IsInPit,
                    Graphics_CurrentSectorIndex = g.CurrentSectorIndex,
                    Graphics_LastSectorTime = g.LastSectorTime,
                    Graphics_NumberOfLaps = g.NumberOfLaps,
                    Graphics_TyreCompound = SanitizeAscii(g.TyreCompound),
                    Graphics_ReplayTimeMultiplier = g.ReplayTimeMultiplier,
                    Graphics_NormalizedCarPosition = g.NormalizedCarPosition,
                    Graphics_PenaltyTime = g.PenaltyTime,
                    Graphics_Flag = (int)g.Flag,
                    Graphics_IdealLineOn = g.IdealLineOn,
                    Graphics_IsInPitLane = g.IsInPitLane,
                    Graphics_SurfaceGrip = g.SurfaceGrip,
                    Graphics_MandatoryPitDone = g.MandatoryPitDone,
                    Graphics_WindSpeed = g.WindSpeed,
                    Graphics_WindDirection = g.WindDirection,
                    // windDirection ends AC's graphics page at 296 bytes. Graphics_ActiveCars
                    // and Graphics_Penalty are gone because those fields never existed in AC
                    // -- ActiveCars was in fact reading the car's X coordinate -- and so is
                    // the whole ACC tail from IsSetupMenuVisible to StrategyTyreSet.

                    // ---- StaticInfo (all fields, with sanitized strings, if available) ----
                    StaticInfo_SMVersion = s.HasValue ? SanitizeAscii(s.Value.SMVersion) : null,
                    StaticInfo_ACVersion = s.HasValue ? SanitizeAscii(s.Value.ACVersion) : null,
                    StaticInfo_NumberOfSessions = s?.NumberOfSessions ?? 0,
                    StaticInfo_NumCars = s?.NumCars ?? 0,
                    StaticInfo_CarModel = s.HasValue ? SanitizeAscii(s.Value.CarModel) : null,
                    StaticInfo_Track = s.HasValue ? SanitizeAscii(s.Value.Track) : null,
                    StaticInfo_PlayerName = s.HasValue ? Sanitize(s.Value.PlayerName) : null,
                    StaticInfo_PlayerSurname = s.HasValue ? Sanitize(s.Value.PlayerSurname) : null,
                    StaticInfo_PlayerNick = s.HasValue ? Sanitize(s.Value.PlayerNick) : null,
                    StaticInfo_SectorCount = s?.SectorCount ?? 0,
                    StaticInfo_MaxTorque = s?.MaxTorque ?? 0,
                    StaticInfo_MaxPower = s?.MaxPower ?? 0,
                    StaticInfo_MaxRpm = s?.MaxRpm ?? 0,
                    StaticInfo_MaxFuel = s?.MaxFuel ?? 0,
                    StaticInfo_SuspensionMaxTravel0 = s?.SuspensionMaxTravel?[0] ?? 0,
                    StaticInfo_SuspensionMaxTravel1 = s?.SuspensionMaxTravel?[1] ?? 0,
                    StaticInfo_SuspensionMaxTravel2 = s?.SuspensionMaxTravel?[2] ?? 0,
                    StaticInfo_SuspensionMaxTravel3 = s?.SuspensionMaxTravel?[3] ?? 0,
                    StaticInfo_TyreRadius0 = s?.TyreRadius?[0] ?? 0,
                    StaticInfo_TyreRadius1 = s?.TyreRadius?[1] ?? 0,
                    StaticInfo_TyreRadius2 = s?.TyreRadius?[2] ?? 0,
                    StaticInfo_TyreRadius3 = s?.TyreRadius?[3] ?? 0,
                    StaticInfo_MaxTurboBoost = s?.MaxTurboBoost ?? 0,
                    StaticInfo_Deprecated1 = s?.Deprecated1 ?? 0,
                    StaticInfo_Deprecated2 = s?.Deprecated2 ?? 0,
                    StaticInfo_PenaltiesEnabled = s?.PenaltiesEnabled ?? 0,
                    StaticInfo_AidFuelRate = s?.AidFuelRate ?? 0,
                    StaticInfo_AidTireRate = s?.AidTireRate ?? 0,
                    StaticInfo_AidMechanicalDamage = s?.AidMechanicalDamage ?? 0,
                    StaticInfo_AidAllowTyreBlankets = s?.AidAllowTyreBlankets ?? 0,
                    StaticInfo_AidStability = s?.AidStability ?? 0,
                    StaticInfo_AidAutoClutch = s?.AidAutoClutch ?? 0,
                    StaticInfo_AidAutoBlip = s?.AidAutoBlip ?? 0,
                    StaticInfo_HasDRS = s?.HasDRS ?? 0,
                    StaticInfo_HasERS = s?.HasERS ?? 0,
                    StaticInfo_HasKERS = s?.HasKERS ?? 0,
                    StaticInfo_KersMaxJoules = s?.KersMaxJoules ?? 0,
                    StaticInfo_EngineBrakeSettingsCount = s?.EngineBrakeSettingsCount ?? 0,
                    StaticInfo_ErsPowerControllerCount = s?.ErsPowerControllerCount ?? 0,
                    StaticInfo_TrackSPlineLength = s?.TrackSPlineLength ?? 0,
                    StaticInfo_TrackConfiguration = s.HasValue ? SanitizeAscii(s.Value.TrackConfiguration) : null,
                    StaticInfo_ErsMaxJ = s?.ErsMaxJ ?? 0,
                    StaticInfo_IsTimedRace = s?.IsTimedRace ?? 0,
                    StaticInfo_HasExtraLap = s?.HasExtraLap ?? 0,
                    StaticInfo_CarSkin = s.HasValue ? SanitizeAscii(s.Value.CarSkin) : null,
                    StaticInfo_ReversedGridPositions = s?.ReversedGridPositions ?? 0,
                    StaticInfo_PitWindowStart = s?.PitWindowStart ?? 0,
                    StaticInfo_PitWindowEnd = s?.PitWindowEnd ?? 0,
                    StaticInfo_IsOnline = s?.IsOnline ?? 0,
                    // DryTyresName/WetTyresName dropped: ACC-only, and the probe read the
                    // static page as all-zero from offset 621 onward.
                };

                // Straight to UTF-8 bytes: one write for the line, one for the newline.
                // _jsonOptions installs converters that clamp NaN/Infinity and sanitize
                // strings, so this cannot throw on bad float or char data. Previously it
                // could, and the catch below swallowed it -- so a misaligned read did not
                // just corrupt lines, it silently dropped them.
                byte[] json = JsonSerializer.SerializeToUtf8Bytes(data, _jsonOptions);
                _out.Write(json, 0, json.Length);
                _out.WriteByte((byte)'\n');
                _written++;

                if (++_sinceFlush >= FlushEveryLines)
                {
                    _sinceFlush = 0;
                    _out.Flush();
                }
            }
            catch (Exception ex)
            {
                _serializeErrors++;
                if (_serializeErrors <= 5)
                    Console.WriteLine($"Serialization error #{_serializeErrors}: {ex.GetType().Name}: {ex.Message}");
            }
        }
    }

    // ==================================================================
    //  JSON -- converters that make serialization unable to fail
    // ==================================================================

    private const string LoggerVersion = "3.0";

    private static JsonSerializerOptions BuildJsonOptions()
    {
        var o = new JsonSerializerOptions
        {
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
            WriteIndented = false,
        };
        o.Converters.Add(new SafeFloatConverter());
        o.Converters.Add(new SafeDoubleConverter());
        o.Converters.Add(new SafeStringConverter());
        return o;
    }

    /// <summary>
    /// System.Text.Json throws on NaN and Infinity by default. An uninitialised or
    /// misaligned page produces both, and that throw used to take the entire line with
    /// it. Clamping to 0 and counting it costs one field instead of one frame.
    /// (Writing the literals as "NaN" strings was the other option, but that would
    /// break every consumer that parses these as numbers.)
    /// </summary>
    private sealed class SafeFloatConverter : JsonConverter<float>
    {
        public override float Read(ref Utf8JsonReader r, Type t, JsonSerializerOptions o) => r.GetSingle();
        public override void Write(Utf8JsonWriter w, float v, JsonSerializerOptions o)
        {
            if (float.IsNaN(v) || float.IsInfinity(v))
            {
                Interlocked.Increment(ref _nonFiniteFloats);
                w.WriteNumberValue(0);
            }
            else w.WriteNumberValue(v);
        }
    }

    private sealed class SafeDoubleConverter : JsonConverter<double>
    {
        public override double Read(ref Utf8JsonReader r, Type t, JsonSerializerOptions o) => r.GetDouble();
        public override void Write(Utf8JsonWriter w, double v, JsonSerializerOptions o)
        {
            if (double.IsNaN(v) || double.IsInfinity(v))
            {
                Interlocked.Increment(ref _nonFiniteFloats);
                w.WriteNumberValue(0);
            }
            else w.WriteNumberValue(v);
        }
    }

    /// <summary>
    /// Backstop for strings. Every string in the payload is already wrapped in
    /// Sanitize, but this makes it structural: any string that reaches the writer is
    /// clean whether or not a newly added field remembers to wrap itself. Sanitize is
    /// idempotent, so the second pass over an already-clean string costs nothing and
    /// flags nothing.
    /// </summary>
    private sealed class SafeStringConverter : JsonConverter<string>
    {
        public override string Read(ref Utf8JsonReader r, Type t, JsonSerializerOptions o) => r.GetString() ?? "";
        public override void Write(Utf8JsonWriter w, string v, JsonSerializerOptions o) => w.WriteStringValue(Sanitize(v));
    }

    // ==================================================================
    //  CLI plumbing
    // ==================================================================

    private static void PrintUsage()
    {
        Console.WriteLine(
@"Usage: AcTelemetryLogger [output] [options]

  output              output file, or a directory to put a generated name in.
                      Default: telemetry_ac_<track>_<car>_<stamp>.ndjson.gz here.

  --out <path>        same as the positional argument
  --plain             uncompressed NDJSON (default is gzip -- roughly 10x smaller,
                      and the Rust reader already handles .gz transparently)
  --selftest          run the shared-memory layout check, write a report, exit
  --dump-graphics     print the contested page regions (graphics 240..304, static
                      664..704, physics 556..592) decoded as int32 and float, every
                      2 s until stopped. Nothing is logged. Use it to confirm the
                      layout from live memory: carCoordinates @252..260 must track
                      the car, surfaceGrip @280 sits near 0.98 on a dry track, and
                      everything from 296 on must stay 0. (--dump-pages is the same
                      thing; it dumps all three pages, not just graphics.)
  --physics-ms <n>    physics poll interval, default 10
  --graphics-ms <n>   graphics poll interval, default 10
  --static-ms <n>     static-info poll interval, default 1000
  -h, --help          this text

Every frame carries the full field set; nothing is pruned. Keys are the raw AC page
names so a cross-sim field mapping can be built from real data later.");
    }

    private static int ParseIntArg(string[] args, ref int i, int fallback)
    {
        if (i + 1 < args.Length &&
            int.TryParse(args[i + 1], NumberStyles.Integer, CultureInfo.InvariantCulture, out int v) && v > 0)
        {
            i++;
            return v;
        }
        Console.WriteLine($"Ignoring malformed value for {args[i]}; keeping {fallback}");
        return fallback;
    }

    private static void OpenOutput(string? outArg, SelfTestReport report)
    {
        string path;
        if (string.IsNullOrWhiteSpace(outArg))
            path = DefaultFileName(report, Directory.GetCurrentDirectory());
        else if (Directory.Exists(outArg))
            path = DefaultFileName(report, outArg);
        else
        {
            path = outArg;
            if (_gzip && !path.EndsWith(".gz", StringComparison.OrdinalIgnoreCase))
                path += ".gz";
        }

        string? dir = Path.GetDirectoryName(Path.GetFullPath(path));
        if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

        // Deliberately not appending, which is what the old version did. Two reasons:
        // a gzip member appended after a truncated one makes the whole file unreadable
        // rather than just the tail, and one file per session keeps track/car/setup
        // constant across the file so the analysis side can trust the static fields.
        path = Unique(path);
        _outputPath = path;

        _fileStream = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.Read, 64 * 1024);
        if (_gzip)
        {
            // leaveOpen so shutdown can fsync the file after the gzip trailer is written.
            _gzipStream = new GZipStream(_fileStream, CompressionLevel.Optimal, leaveOpen: true);
            _out = _gzipStream;
        }
        else _out = _fileStream;

        TryWriteJson(SidecarPath(), report.ToMetaObject(_outputPath, _gzip));
    }

    private static string DefaultFileName(SelfTestReport report, string dir)
    {
        var parts = new List<string> { "telemetry_ac" };
        string track = SanitizeFileName(report.Track);
        string car = SanitizeFileName(report.CarModel);
        if (track.Length > 0) parts.Add(track);
        if (car.Length > 0) parts.Add(car);
        parts.Add(DateTime.Now.ToString("yyyyMMdd_HHmmss", CultureInfo.InvariantCulture));
        return Path.Combine(dir, string.Join("_", parts) + ".ndjson" + (_gzip ? ".gz" : ""));
    }

    private static string SanitizeFileName(string? s)
    {
        string clean = SanitizeCore(s, out _);
        if (clean.Length == 0) return "";
        var sb = new StringBuilder(clean.Length);
        foreach (char c in clean)
            sb.Append(char.IsLetterOrDigit(c) || c == '-' ? c : '_');
        return sb.ToString().Trim('_');
    }

    private static string Unique(string path)
    {
        if (!File.Exists(path)) return path;

        string dir = Path.GetDirectoryName(path) ?? "";
        string name = Path.GetFileName(path);
        string suffix = "";
        foreach (string s in new[] { ".ndjson.gz", ".ndjson", ".json.gz", ".gz" })
        {
            if (name.EndsWith(s, StringComparison.OrdinalIgnoreCase))
            {
                suffix = name.Substring(name.Length - s.Length);
                name = name.Substring(0, name.Length - s.Length);
                break;
            }
        }
        if (suffix.Length == 0)
        {
            suffix = Path.GetExtension(name);
            name = Path.GetFileNameWithoutExtension(name);
        }
        for (int n = 2; ; n++)
        {
            string candidate = Path.Combine(dir, $"{name}_{n}{suffix}");
            if (!File.Exists(candidate)) return candidate;
        }
    }

    private static string SidecarPath() => _outputPath + ".meta.json";

    private static void TryWriteJson(string path, object payload)
    {
        try
        {
            // No string converter here: it caps length at MaxStringLength, which would
            // truncate the hex dumps that make this file worth reading.
            var opts = new JsonSerializerOptions { WriteIndented = true };
            opts.Converters.Add(new SafeFloatConverter());
            opts.Converters.Add(new SafeDoubleConverter());
            File.WriteAllText(path, JsonSerializer.Serialize(payload, opts));
        }
        catch (Exception ex)
        {
            Console.WriteLine($"Could not write {path}: {ex.Message}");
        }
    }

    private static void StartKeyWatcher()
    {
        var t = new Thread(() =>
        {
            try
            {
                if (Console.IsInputRedirected) return;
                Console.ReadKey(intercept: true);
                Shutdown("key pressed");
                _exit.Set();
            }
            catch
            {
                // No console to read from. Ctrl+C and process exit still work.
            }
        })
        { IsBackground = true, Name = "key-watcher" };
        t.Start();
    }

    private static System.Threading.Timer StartProgressReporter() => new System.Threading.Timer(_ =>
    {
        // phys is first because it is the one number that says whether anything is
        // reaching us at all. Zero here with the logger running means the shared memory
        // never attached -- and AssettoCorsa now prints why.
        Console.Write($"\r  phys {_ac?.PhysicsEventsRaised ?? 0,8}   " +
                      $"frames {Interlocked.Read(ref _written),8}   " +
                      $"waiting-for-pos {Interlocked.Read(ref _skippedNoPosition),7}   " +
                      $"dup {Interlocked.Read(ref _skippedDuplicate),7}   " +
                      $"bad-str {Interlocked.Read(ref _sanitizedStrings),6}   " +
                      $"bad-float {Interlocked.Read(ref _nonFiniteFloats),6}  ");
    }, null, 2000, 2000);

    private static void Shutdown(string reason)
    {
        if (Interlocked.Exchange(ref _shutdownStarted, 1) != 0) return;

        try { _ac?.Stop(); } catch { /* nothing useful to do here */ }

        // The order here is not optional under gzip. Deflate buffers internally, so
        // disposing the GZipStream is what emits the final block and the trailer;
        // losing that costs the tail of the whole file, not one line. Then fsync.
        lock (_lock)
        {
            try { _out?.Flush(); } catch { }
            try { _gzipStream?.Dispose(); } catch { }
            try { _fileStream?.Flush(flushToDisk: true); } catch { }
            try { _fileStream?.Dispose(); } catch { }
            _out = null; _gzipStream = null; _fileStream = null;
        }

        Console.WriteLine();
        Console.WriteLine($"Stopped: {reason}");
        long physEvents = _ac?.PhysicsEventsRaised ?? 0;
        long gfxEvents = _ac?.GraphicsEventsRaised ?? 0;
        Console.WriteLine($"  physics reads         {physEvents}");
        Console.WriteLine($"  graphics reads        {gfxEvents}");
        Console.WriteLine($"  frames written        {_written}");
        Console.WriteLine($"  skipped, no position  {_skippedNoPosition}");
        Console.WriteLine($"  skipped, duplicate    {_skippedDuplicate}");
        Console.WriteLine($"  serialization errors  {_serializeErrors}");
        Console.WriteLine($"  strings sanitized     {_sanitizedStrings}{(_sanitizedStrings > 0 ? "   <-- layout mismatch; see the sidecar" : "")}");
        Console.WriteLine($"  non-finite floats     {_nonFiniteFloats}{(_nonFiniteFloats > 0 ? "   <-- clamped to 0" : "")}");

        // A zero-frame run used to end here with no explanation at all. The two counters
        // above now separate the only two possible causes, so say which one it was.
        if (_written == 0)
        {
            Console.WriteLine();
            if (physEvents == 0)
                Console.WriteLine("  Nothing was captured because the physics page was never read: the shared\n" +
                                  "  memory never attached. The connect error is printed above -- if there is no\n" +
                                  "  such line, AC was not running. Try --dump-graphics with AC on track.");
            else if (_skippedNoPosition > 0)
                Console.WriteLine($"  Nothing was captured although {physEvents} physics reads arrived: every frame was\n" +
                                   "  held back waiting for a car position. That means graphics.CarCoordinates\n" +
                                   "  stayed all-zero -- you were in the menus, or the graphics layout is wrong.\n" +
                                   "  Check the self-test's CarCoordinates line, or run --dump-graphics.");
            else
                Console.WriteLine($"  Nothing was captured although {physEvents} physics reads arrived and nothing was\n" +
                                   "  skipped. That combination should be impossible -- send the sidecar over.");
        }

        if (_outputPath.Length > 0)
        {
            // GZipStream writes its header lazily, so a session that captured nothing
            // leaves a zero-byte file that is not a valid gzip stream -- which would
            // fail the reader rather than read as empty. Nothing was lost, so bin it.
            bool removed = false;
            if (_written == 0)
            {
                try
                {
                    var empty = new FileInfo(_outputPath);
                    if (empty.Exists && empty.Length == 0)
                    {
                        File.Delete(_outputPath);
                        removed = true;
                    }
                }
                catch { }
            }

            if (removed)
            {
                Console.WriteLine($"  output                (none -- no frames captured, removed empty {Path.GetFileName(_outputPath)})");
            }
            else
            {
                Console.WriteLine($"  output                {_outputPath}");
                try
                {
                    var fi = new FileInfo(_outputPath);
                    if (fi.Exists) Console.WriteLine($"  size                  {fi.Length:N0} bytes");
                }
                catch { }
            }

            if (_report != null) TryWriteJson(SidecarPath(), _report.ToMetaObject(removed ? null : _outputPath, _gzip));
        }
    }

    // ==================================================================
    //  Shared-memory layout self-test
    // ==================================================================

    private sealed class PageProbe
    {
        public string Name = "";
        public int StructSize;
        public bool Opened;
        public string? Error;
        public long ViewCapacity;
        public int LastNonZeroOffset = -1;
        public byte[]? Bytes;

        // AC leaves anything it does not publish as zero, so the last non-zero byte is
        // a lower bound on how much of the page is real. The view capacity cannot tell
        // us: the OS rounds it up to a page (4096), whatever the struct size.
        public int PublishedBytes => LastNonZeroOffset + 1;
        public bool TailLooksUnpublished => Opened && PublishedBytes < StructSize;
    }

    private sealed class Check
    {
        public string Name = "";
        public string Value = "";
        public bool Pass;
        public bool Fatal = true;
        public string? Note;
    }

    private sealed class SelfTestReport
    {
        public PageProbe Physics = new(), Graphics = new(), Static = new();
        public List<Check> Checks = new();
        public string Track = "", CarModel = "", AcVersion = "", SmVersion = "";
        public bool PacketIdAdvanced;
        public bool AcRunning;

        public bool AnyFail => Checks.Any(c => !c.Pass && c.Fatal);

        public void Print()
        {
            if (!AcRunning)
            {
                Console.WriteLine("  Assetto Corsa is not running -- no acpmf_* pages to read.");
                foreach (var p in new[] { Physics, Graphics, Static })
                    Console.WriteLine($"    {p.Name,-22} {p.Error}");
                Console.WriteLine("  Layout cannot be checked until AC is up with a session loaded.");
                Console.WriteLine("----------------------------------------------------------------------");
                return;
            }

            foreach (var p in new[] { Physics, Graphics, Static })
            {
                Console.WriteLine(p.Opened
                    ? $"  {p.Name,-22} struct {p.StructSize,5} B   published >={p.PublishedBytes,5} B   view {p.ViewCapacity,6} B"
                    : $"  {p.Name,-22} NOT OPEN: {p.Error}");
            }

            Console.WriteLine();
            foreach (var c in Checks)
            {
                Console.WriteLine($"  [{(c.Pass ? "PASS" : c.Fatal ? "FAIL" : "WARN")}] {c.Name,-32} {c.Value}");
                if (!c.Pass && c.Note != null) Console.WriteLine($"         {c.Note}");
            }
            Console.WriteLine("----------------------------------------------------------------------");
        }

        public object ToMetaObject(string? outputPath, bool gzip) => new
        {
            logger = "AcTelemetryLogger",
            logger_version = LoggerVersion,
            written_utc = DateTime.UtcNow.ToString("o", CultureInfo.InvariantCulture),
            output = outputPath,
            gzip,
            ac_running = AcRunning,
            sm_version = SmVersion,
            ac_version = AcVersion,
            track = Track,
            car_model = CarModel,
            packet_id_advances = PacketIdAdvanced,
            any_fatal_failure = AnyFail,
            pages = new[] { PageMeta(Physics), PageMeta(Graphics), PageMeta(Static) },
            checks = Checks.Select(c => new { name = c.Name, pass = c.Pass, fatal = c.Fatal, value = c.Value, note = c.Note }).ToArray(),
            // Raw bytes for the regions where the layout was in doubt -- the tail of each
            // page, where AC's fields stop and ACC's used to be assumed. If a check
            // failed, this is what identifies where the real layout diverges. Same
            // regions --dump-graphics decodes, kept here so a single sidecar is enough to
            // diagnose a run after the fact.
            suspect_regions = new
            {
                graphics_240_to_304 = HexDump(Graphics.Bytes, 240, 64),
                static_664_to_704 = HexDump(Static.Bytes, 664, 40),
                physics_556_to_592 = HexDump(Physics.Bytes, 556, 36),
            },
            counters = new
            {
                // physics_events_received distinguishes "the timer never fired / the page
                // never read" from "frames arrived but every one was skipped". Both used
                // to look identical: frames_written 0 with no other explanation.
                physics_events_received = _ac?.PhysicsEventsRaised ?? 0,
                graphics_events_received = _ac?.GraphicsEventsRaised ?? 0,
                frames_written = _written,
                skipped_no_position = _skippedNoPosition,
                skipped_duplicate = _skippedDuplicate,
                serialization_errors = _serializeErrors,
                strings_sanitized = _sanitizedStrings,
                non_finite_floats = _nonFiniteFloats,
            },
            notes = new[]
            {
                "Field names are the raw AC shared-memory page names. Nothing is pruned.",
                "SCHEMA CHANGE vs logger versions before " + LoggerVersion + ": the ACC-only columns were REMOVED, not written as null. Physics dropped p2p_activation..abs_vibrations, graphics dropped active_cars/penalty/is_setup_menu_visible..strategy_tyre_set, static dropped dry_tyres_name/wet_tyres_name -- roughly 60 columns. AC 1.14 never publishes those bytes, so they were being logged as zeros indistinguishable from real readings. Struct sizes are now 580/296/688, matching what AC publishes.",
                "graphics.car_coordinates now reads from offset 252 (AC's real layout). Before this version it read from 256 under an assumed ACC multi-car block, so position_x/y/z in older files are actually y/z/penalty_time and position_z was always 0. Older files cannot be corrected by renaming -- their z is not in the file.",
                "strings_sanitized > 0 means the struct layout does not match the page this AC build publishes; fields at or past the first failing offset are not trustworthy.",
                "non_finite_floats counts NaN/Infinity clamped to 0 on write; a zero in the data may therefore be a clamp, not a reading.",
                "published_bytes_estimate is a lower bound from the last non-zero byte, not an exact page size. A tail of zeros is normal: AC leaves fields it has nothing to say about (pit window, mandatory pit, online-only fields) at 0.",
                "static.is_online is unconfirmed for AC 1.14 -- it may sit 4 bytes past the page and read 0 forever. It is last, so nothing behind it is affected either way.",
            },
        };

        private static object PageMeta(PageProbe p) => new
        {
            name = p.Name,
            opened = p.Opened,
            error = p.Error,
            struct_size = p.StructSize,
            view_capacity = p.ViewCapacity,
            last_nonzero_offset = p.LastNonZeroOffset,
            published_bytes_estimate = p.PublishedBytes,
            tail_looks_unpublished = p.TailLooksUnpublished,
        };
    }

    private static PageProbe ProbePage(string name, int structSize)
    {
        var pr = new PageProbe { Name = name, StructSize = structSize };
        try
        {
            using var mmf = MemoryMappedFile.OpenExisting(name, MemoryMappedFileRights.Read);
            using var acc = mmf.CreateViewAccessor(0, 0, MemoryMappedFileAccess.Read);
            pr.Opened = true;
            pr.ViewCapacity = acc.Capacity;

            int scan = (int)Math.Min(acc.Capacity, Math.Max(structSize, 2048));
            var buf = new byte[scan];
            acc.ReadArray(0, buf, 0, scan);
            pr.Bytes = buf;
            for (int i = scan - 1; i >= 0; i--)
            {
                if (buf[i] != 0) { pr.LastNonZeroOffset = i; break; }
            }
        }
        catch (Exception ex)
        {
            pr.Error = $"{ex.GetType().Name}: {ex.Message}";
        }
        return pr;
    }

    private static T? MarshalFrom<T>(byte[]? buf) where T : struct
    {
        if (buf == null) return null;
        int size = Marshal.SizeOf<T>();
        if (buf.Length < size) return null;   // short read: refuse rather than marshal junk

        var h = GCHandle.Alloc(buf, GCHandleType.Pinned);
        try { return Marshal.PtrToStructure<T>(h.AddrOfPinnedObject()); }
        catch { return null; }
        finally { h.Free(); }
    }

    private static string HexDump(byte[]? buf, int offset, int length)
    {
        if (buf == null || offset < 0 || offset >= buf.Length) return "";
        int n = Math.Min(length, buf.Length - offset);
        var sb = new StringBuilder(n * 3);
        for (int i = 0; i < n; i++)
        {
            if (i > 0) sb.Append(' ');
            sb.Append(buf[offset + i].ToString("x2", CultureInfo.InvariantCulture));
        }
        return sb.ToString();
    }

    private static Check Num(string name, double v, double lo, double hi, bool fatal = true, string? note = null) => new()
    {
        Name = name,
        Value = double.IsNaN(v) || double.IsInfinity(v) ? v.ToString(CultureInfo.InvariantCulture)
                                                        : v.ToString("0.####", CultureInfo.InvariantCulture),
        Pass = !double.IsNaN(v) && !double.IsInfinity(v) && v >= lo && v <= hi,
        Fatal = fatal,
        Note = note ?? $"expected {lo} .. {hi}",
    };

    private static Check Str(string name, string? raw, bool fatal = true)
    {
        string clean = SanitizeCore(raw, out bool suspicious);
        return new Check
        {
            Name = name,
            Value = suspicious ? $"\"{clean}\"  <-- junk past the terminator" : $"\"{clean}\"",
            Pass = !suspicious && clean.Length > 0,
            Fatal = fatal,
            Note = suspicious
                ? "these bytes are not a string; the real layout diverges at or before this offset"
                : "empty -- either not populated yet, or the offset is wrong",
        };
    }

    // Page sizes AC 1.14.1 / shared memory 1.7 actually publishes. Confirmed against the
    // sim_info.py bundled with this install, the copy inside Custom Shaders Patch, Kunos'
    // own SharedFileOut.h, and the live probe (see the header comment in Graphics.cs).
    // A mismatch means a struct was edited and every offset past the edit has moved.
    private const int AcPhysicsPageSize = 580;
    private const int AcGraphicsPageSize = 296;
    private const int AcStaticPageSize = 688;

    private static Check SizeCheck(string name, int actual, int expected) => new()
    {
        Name = name,
        Value = $"{actual} B (AC publishes {expected} B)",
        Pass = actual == expected,
        Fatal = true,
        Note = "struct size no longer matches AC's page; every offset past the edited field has moved",
    };

    /// <summary>
    /// carCoordinates is the field that proves the graphics layout, so this is both the
    /// layout check and the check that PositionX/Y/Z are real.
    ///
    /// It replaces a fatal "graphics.ActiveCars @252 must be 0..128" check on a field AC
    /// does not have -- which is why every single run reported any_fatal_failure. The
    /// value it rejected, 1132052166, is the float 249.745: the car's X coordinate at Red
    /// Bull Ring, sitting exactly where AC puts carCoordinates[0].
    /// </summary>
    private static Check CarCoords(float[]? c)
    {
        if (c == null || c.Length < 3)
        {
            return new Check
            {
                Name = "graphics.CarCoordinates @252",
                Value = c == null ? "null" : $"length {c.Length}",
                Pass = false,
                Fatal = true,
                Note = "marshalled as null or short -- the Graphics struct is wrong",
            };
        }

        bool finite = true, inRange = true;
        for (int i = 0; i < 3; i++)
        {
            if (float.IsNaN(c[i]) || float.IsInfinity(c[i])) finite = false;
            else if (Math.Abs(c[i]) > 20000f) inRange = false;
        }
        bool plausible = finite && inRange;
        bool allZero = c[0] == 0 && c[1] == 0 && c[2] == 0;

        return new Check
        {
            Name = "graphics.CarCoordinates @252",
            Value = string.Format(CultureInfo.InvariantCulture, "[{0:0.###}, {1:0.###}, {2:0.###}]", c[0], c[1], c[2]),
            Pass = plausible && !allZero,
            // In range but all zero just means the car is not placed on track yet, which
            // is not a layout problem.
            Fatal = !plausible,
            Note = !plausible
                ? "world position in metres; these are not plausible coordinates, so the graphics layout diverges at or before offset 252"
                : "all zero -- car not on track yet; re-check once you are driving",
        };
    }

    // ==================================================================
    //  --dump-graphics: decode the contested offsets from live memory
    // ==================================================================

    private static readonly Dictionary<int, string> GraphicsDumpLabels = new()
    {
        [240] = "(tyreCompound tail)",
        [244] = "replayTimeMultiplier",
        [248] = "normalizedCarPosition   0..1",
        [252] = "carCoordinates[0]  X    <- tracks the car",
        [256] = "carCoordinates[1]  Y    <- tracks the car",
        [260] = "carCoordinates[2]  Z    <- tracks the car",
        [264] = "penaltyTime",
        [268] = "flag",
        [272] = "idealLineOn",
        [276] = "isInPitLane",
        [280] = "surfaceGrip             <- ~0.98 on a dry track",
        [284] = "mandatoryPitDone",
        [288] = "windSpeed",
        [292] = "windDirection           <- LAST field; page ends at 296",
        [296] = "(past the end of AC's page -- expect 0)",
        [300] = "(past the end of AC's page -- expect 0)",
    };

    private static readonly Dictionary<int, string> StaticDumpLabels = new()
    {
        [664] = "(carSkin tail)",
        [668] = "(carSkin tail)",
        [672] = "reversedGridPositions",
        [676] = "pitWindowStart",
        [680] = "pitWindowEnd",
        [684] = "isOnline                <- UNCONFIRMED for 1.14; 1 in an online session",
        [688] = "(past the end -- expect 0)",
        [692] = "(past the end -- expect 0)",
        [696] = "(past the end -- expect 0)",
        [700] = "(past the end -- expect 0)",
    };

    private static readonly Dictionary<int, string> PhysicsDumpLabels = new()
    {
        [556] = "tyreContactHeading[3].Y",
        [560] = "tyreContactHeading[3].Z",
        [564] = "brakeBias",
        [568] = "localVelocity[0]",
        [572] = "localVelocity[1]",
        [576] = "localVelocity[2]        <- LAST field; page ends at 580",
        [580] = "(past the end of AC's page -- expect 0)",
        [584] = "(past the end of AC's page -- expect 0)",
        [588] = "(past the end of AC's page -- expect 0)",
    };

    /// <summary>
    /// Prints the regions where the layout was in doubt, decoded as both int32 and float,
    /// so live memory settles it in one lap without a rebuild. Repeats until stopped
    /// because the point is watching values move: carCoordinates must track the car.
    /// </summary>
    private static int RunDumpGraphics()
    {
        Console.WriteLine();
        Console.WriteLine("-- live page dump ----------------------------------------------------");
        Console.WriteLine("  Drive a lap and watch. carCoordinates must track the car, surfaceGrip");
        Console.WriteLine("  should sit near 0.98 on a dry track, and everything at or past the");
        Console.WriteLine("  'past the end' rows should stay 0. Ctrl+C or any key stops.");

        bool interactive = !Console.IsInputRedirected;
        var stop = new ManualResetEventSlim(false);
        Console.CancelKeyPress += (_, e) => { e.Cancel = true; stop.Set(); };

        int snapshot = 0;
        while (!stop.IsSet)
        {
            var g = ProbePage(GraphicsPage, Marshal.SizeOf<Graphics>());
            var s = ProbePage(StaticPage, Marshal.SizeOf<StaticInfo>());
            var p = ProbePage(PhysicsPage, Marshal.SizeOf<Physics>());

            if (!g.Opened && !s.Opened && !p.Opened)
            {
                Console.WriteLine();
                Console.WriteLine($"  Assetto Corsa is not running -- no acpmf_* pages: {g.Error}");
                return 1;
            }

            Console.WriteLine();
            Console.WriteLine($"===== snapshot {++snapshot}  {DateTime.Now.ToString("HH:mm:ss", CultureInfo.InvariantCulture)} =====");
            DumpRegion("graphics", g, 240, 304, GraphicsDumpLabels);
            DumpRegion("static", s, 664, 704, StaticDumpLabels);
            DumpRegion("physics", p, 556, 592, PhysicsDumpLabels);

            if (interactive && Console.KeyAvailable) { Console.ReadKey(intercept: true); break; }
            stop.Wait(2000);
        }

        Console.WriteLine();
        Console.WriteLine("Dump stopped.");
        return 0;
    }

    private static void DumpRegion(string page, PageProbe pr, int from, int to, Dictionary<int, string> labels)
    {
        Console.WriteLine();
        if (!pr.Opened || pr.Bytes == null)
        {
            Console.WriteLine($"  {page,-9} NOT OPEN: {pr.Error}");
            return;
        }

        Console.WriteLine($"  {page,-9} struct {pr.StructSize} B, last non-zero byte {pr.LastNonZeroOffset}");
        Console.WriteLine($"    {"off",4}  {"hex",-11}  {"int32",12}  {"float",14}   field");
        for (int off = from; off + 4 <= to && off + 4 <= pr.Bytes.Length; off += 4)
        {
            int i = BitConverter.ToInt32(pr.Bytes, off);
            float f = BitConverter.ToSingle(pr.Bytes, off);
            string fs = float.IsNaN(f) || float.IsInfinity(f)
                ? f.ToString(CultureInfo.InvariantCulture)
                : f.ToString("0.#####", CultureInfo.InvariantCulture);
            labels.TryGetValue(off, out string? label);
            Console.WriteLine($"    {off,4}  {HexDump(pr.Bytes, off, 4),-11}  {i,12}  {fs,14}   {label}");
        }
    }

    private static SelfTestReport RunSelfTest()
    {
        var r = new SelfTestReport();
        Console.WriteLine();
        Console.WriteLine("-- shared-memory layout self-test ------------------------------------");

        int physSize = Marshal.SizeOf<Physics>();
        r.Physics = ProbePage(PhysicsPage, physSize);
        r.Graphics = ProbePage(GraphicsPage, Marshal.SizeOf<Graphics>());
        r.Static = ProbePage(StaticPage, Marshal.SizeOf<StaticInfo>());

        if (!r.Physics.Opened && !r.Graphics.Opened && !r.Static.Opened)
        {
            r.AcRunning = false;
            return r;
        }
        r.AcRunning = true;

        var p = MarshalFrom<Physics>(r.Physics.Bytes);
        var g = MarshalFrom<Graphics>(r.Graphics.Bytes);
        var s = MarshalFrom<StaticInfo>(r.Static.Bytes);

        // Sample physics twice: if PacketId never moves on this build, duplicate
        // suppression would silently discard every frame, so it stays off.
        if (p.HasValue)
        {
            int first = p.Value.PacketId;
            Thread.Sleep(150);
            var again = MarshalFrom<Physics>(ProbePage(PhysicsPage, physSize).Bytes);
            r.PacketIdAdvanced = again.HasValue && again.Value.PacketId != first;
        }

        if (s.HasValue)
        {
            var sv = s.Value;
            r.SmVersion = SanitizeCore(sv.SMVersion, out _);
            r.AcVersion = SanitizeCore(sv.ACVersion, out _);
            r.Track = SanitizeCore(sv.Track, out _);
            r.CarModel = SanitizeCore(sv.CarModel, out _);

            r.Checks.Add(Str("static.SMVersion @0", sv.SMVersion));
            r.Checks.Add(Str("static.ACVersion @30", sv.ACVersion));
            r.Checks.Add(Str("static.CarModel @68", sv.CarModel));
            r.Checks.Add(Str("static.Track @134", sv.Track));
            r.Checks.Add(Num("static.TrackSPlineLength @520", sv.TrackSPlineLength, 100, 30000, fatal: false,
                             note: "track length in metres; 0 before a session is loaded"));
            r.Checks.Add(Str("static.CarSkin @604", sv.CarSkin, fatal: false));
        }

        if (g.HasValue)
        {
            var gv = g.Value;
            r.Checks.Add(Num("graphics.Status @4", (int)gv.Status, 0, 3, note: "AC_OFF/REPLAY/LIVE/PAUSE only"));
            r.Checks.Add(Num("graphics.CompletedLaps @132", gv.CompletedLaps, 0, 10000));
            r.Checks.Add(Num("graphics.NormalizedCarPosition @248", gv.NormalizedCarPosition, -0.01, 1.01,
                             note: "spline position, must be 0..1"));
            r.Checks.Add(CarCoords(gv.CarCoordinates));
            r.Checks.Add(Num("graphics.IsInPitLane @276", gv.IsInPitLane, 0, 1, fatal: false));
            r.Checks.Add(Num("graphics.SurfaceGrip @280", gv.SurfaceGrip, 0, 1.5, fatal: false,
                             note: "grip multiplier, ~0.98 on a dry track; 0 before the session starts"));
            r.Checks.Add(Num("graphics.WindDirection @292", gv.WindDirection, -7, 7, fatal: false,
                             note: "last field in AC's graphics page (292..295); radians, 0 when there is no wind"));
        }

        if (p.HasValue)
        {
            var pv = p.Value;
            r.Checks.Add(Num("physics.Gas @4", pv.Gas, 0, 1.01));
            r.Checks.Add(Num("physics.Brake @8", pv.Brake, 0, 1.01));
            r.Checks.Add(Num("physics.SpeedKmh @28", pv.SpeedKmh, -20, 600));
            r.Checks.Add(Num("physics.Gear @16", pv.Gear, 0, 10));
            r.Checks.Add(Num("physics.Heading @208", pv.Heading, -Math.PI * 1.01, Math.PI * 1.01));
            r.Checks.Add(Num("physics.NumberOfTyresOut @244", pv.NumberOfTyresOut, 0, 4));
            r.Checks.Add(Num("physics.LocalVelocity[0] @568", pv.LocalVelocity?[0] ?? float.NaN, -200, 200,
                             fatal: false,
                             note: "last field in AC's physics page (568..579); junk here means the page is shorter than we think"));
        }

        // Struct size against the page size AC actually publishes. This replaces the old
        // per-page "tail is all zero from offset N" warning, which fired on every run for
        // an innocent reason: AC leaves fields it has nothing to say about as zero -- the
        // unused tail of a fixed-size string, wind that is not blowing, a pit window that
        // is not set -- so the last non-zero byte always lands short of the struct. The
        // probe's last_nonzero_offset is still recorded in the sidecar as information.
        r.Checks.Add(SizeCheck("physics struct size", physSize, AcPhysicsPageSize));
        r.Checks.Add(SizeCheck("graphics struct size", Marshal.SizeOf<Graphics>(), AcGraphicsPageSize));
        r.Checks.Add(SizeCheck("static struct size", Marshal.SizeOf<StaticInfo>(), AcStaticPageSize));

        return r;
    }
}
