using System;
using System.IO;
using System.IO.Compression;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using AMS2SharedMemoryNet;

#pragma warning disable CA1416 // MemoryParser is Windows-only – we know we're on Windows.

class Program
{
    // ---------- Helper functions for string extraction ----------
    static string GetStringFromChars(char[] chars)
    {
        int len = Array.IndexOf(chars, '\0');
        return len >= 0 ? new string(chars, 0, len) : new string(chars);
    }

    static string SanitizeFileName(string name)
    {
        foreach (char c in Path.GetInvalidFileNameChars())
            name = name.Replace(c, '_');
        return name;
    }

    static string? GetTrackKey(dynamic page)
    {
        string location = GetStringFromChars(page.mTrackLocation);
        string variation = GetStringFromChars(page.mTrackVariation);

        if (string.IsNullOrEmpty(location))
            return null;

        if (string.IsNullOrEmpty(variation) || variation.Equals(location, StringComparison.OrdinalIgnoreCase))
            return location;

        return $"{location}_{variation}";
    }

    // ---------- DTO with short JSON names ----------
    public sealed class CleanTelemetryFrame
    {
        [JsonPropertyName("ts")] public long Timestamp { get; set; }
        [JsonPropertyName("seq")] public uint SequenceNumber { get; set; }

        [JsonPropertyName("ver")] public uint Version { get; set; }
        [JsonPropertyName("bld")] public uint BuildVersion { get; set; }

        [JsonPropertyName("gs")] public uint GameState { get; set; }
        [JsonPropertyName("ss")] public uint SessionState { get; set; }
        [JsonPropertyName("rs")] public uint RaceState { get; set; }
        [JsonPropertyName("pm")] public uint PitMode { get; set; }
        [JsonPropertyName("ps")] public uint PitSchedule { get; set; }
        [JsonPropertyName("li")] public bool LapInvalidated { get; set; }
        [JsonPropertyName("yf")] public int YellowFlagState { get; set; }

        [JsonPropertyName("loc")] public string? TrackLocation { get; set; }
        [JsonPropertyName("var")] public string? TrackVariation { get; set; }
        [JsonPropertyName("len")] public float TrackLength { get; set; }
        [JsonPropertyName("sec")] public int NumSectors { get; set; }
        [JsonPropertyName("car")] public string? CarName { get; set; }
        [JsonPropertyName("cls")] public string? CarClassName { get; set; }

        [JsonPropertyName("v")] public ViewedParticipant Viewed { get; set; } = new();

        [JsonPropertyName("blt")] public float BestLapTime { get; set; }
        [JsonPropertyName("llt")] public float LastLapTime { get; set; }
        [JsonPropertyName("ct")] public float CurrentTime { get; set; }
        [JsonPropertyName("spt")] public float SplitTime { get; set; }
        [JsonPropertyName("s1")] public float CurrentSector1Time { get; set; }
        [JsonPropertyName("s2")] public float CurrentSector2Time { get; set; }
        [JsonPropertyName("s3")] public float CurrentSector3Time { get; set; }

        [JsonPropertyName("t")] public float Throttle { get; set; }
        [JsonPropertyName("b")] public float Brake { get; set; }
        [JsonPropertyName("s")] public float Steering { get; set; }
        [JsonPropertyName("ut")] public float UnfilteredThrottle { get; set; }
        [JsonPropertyName("ub")] public float UnfilteredBrake { get; set; }
        [JsonPropertyName("us")] public float UnfilteredSteering { get; set; }

        [JsonPropertyName("sp")] public float Speed { get; set; }
        [JsonPropertyName("rpm")] public float Rpm { get; set; }
        [JsonPropertyName("mrpm")] public float MaxRPM { get; set; }
        [JsonPropertyName("g")] public int Gear { get; set; }
        [JsonPropertyName("ng")] public int NumGears { get; set; }

        [JsonPropertyName("absa")] public bool AntiLockActive { get; set; }
        [JsonPropertyName("abss")] public int AntiLockSetting { get; set; }
        [JsonPropertyName("tcs")] public int TractionControlSetting { get; set; }
        [JsonPropertyName("bb")] public float BrakeBias { get; set; }

        [JsonPropertyName("ori")] public float[] Orientation { get; set; } = new float[3];
        [JsonPropertyName("angv")] public float[] AngularVelocity { get; set; } = new float[3];
        [JsonPropertyName("lv")] public float[] LocalVelocity { get; set; } = new float[3];
        [JsonPropertyName("wv")] public float[] WorldVelocity { get; set; } = new float[3];
        [JsonPropertyName("la")] public float[] LocalAcceleration { get; set; } = new float[3];
        [JsonPropertyName("wa")] public float[] WorldAcceleration { get; set; } = new float[3];

        [JsonPropertyName("tl")] public float[] TyreTempLeft { get; set; } = new float[4];
        [JsonPropertyName("tc")] public float[] TyreTempCenter { get; set; } = new float[4];
        [JsonPropertyName("tr")] public float[] TyreTempRight { get; set; } = new float[4];
        [JsonPropertyName("ap")] public float[] AirPressure { get; set; } = new float[4];
        [JsonPropertyName("tw")] public float[] TyreWear { get; set; } = new float[4];
        [JsonPropertyName("trps")] public float[] TyreRPS { get; set; } = new float[4];
        [JsonPropertyName("bt")] public float[] BrakeTempCelsius { get; set; } = new float[4];

        [JsonPropertyName("st")] public float[] SuspensionTravel { get; set; } = new float[4];
        [JsonPropertyName("sv")] public float[] SuspensionVelocity { get; set; } = new float[4];
        [JsonPropertyName("rh")] public float[] RideHeight { get; set; } = new float[4];

        [JsonPropertyName("cs")] public uint CrashState { get; set; }
        [JsonPropertyName("ad")] public float AeroDamage { get; set; }
        [JsonPropertyName("ed")] public float EngineDamage { get; set; }
    }

    public sealed class ViewedParticipant
    {
        [JsonPropertyName("i")] public int Index { get; set; }
        [JsonPropertyName("cl")] public uint CurrentLap { get; set; }
        [JsonPropertyName("lc")] public uint LapsCompleted { get; set; }
        [JsonPropertyName("cs")] public int CurrentSector { get; set; }
        [JsonPropertyName("cld")] public float CurrentLapDistance { get; set; }
        [JsonPropertyName("wp")] public float[] WorldPosition { get; set; } = new float[3];
    }

    // ---------- Mapping ----------
    static CleanTelemetryFrame MapToCleanFrame(dynamic page, long timestamp)
    {
        var clean = new CleanTelemetryFrame
        {
            Timestamp = timestamp,
            SequenceNumber = page.mSequenceNumber,

            Version = page.mVersion,
            BuildVersion = page.mBuildVersionNumber,

            GameState = page.mGameState,
            SessionState = page.mSessionState,
            RaceState = page.mRaceState,

            PitMode = page.mPitMode,
            PitSchedule = page.mPitSchedule,
            LapInvalidated = page.mLapInvalidated != 0,
            YellowFlagState = (int)page.mYellowFlagState,

            TrackLocation = GetStringFromChars(page.mTrackLocation),
            TrackVariation = GetStringFromChars(page.mTrackVariation),
            TrackLength = page.mTrackLength,
            NumSectors = (int)page.mNumSectors,

            CarName = GetStringFromChars(page.mCarName),
            CarClassName = GetStringFromChars(page.mCarClassName),

            BestLapTime = page.mBestLapTime,
            LastLapTime = page.mLastLapTime,
            CurrentTime = page.mCurrentTime,
            SplitTime = page.mSplitTime,
            CurrentSector1Time = page.mCurrentSector1Time,
            CurrentSector2Time = page.mCurrentSector2Time,
            CurrentSector3Time = page.mCurrentSector3Time,

            Throttle = page.mThrottle,
            Brake = page.mBrake,
            Steering = page.mSteering,
            UnfilteredThrottle = page.mUnfilteredThrottle,
            UnfilteredBrake = page.mUnfilteredBrake,
            UnfilteredSteering = page.mUnfilteredSteering,

            Speed = page.mSpeed,
            Rpm = page.mRpm,
            MaxRPM = page.mMaxRPM,
            Gear = (int)page.mGear,
            NumGears = (int)page.mNumGears,

            AntiLockActive = page.mAntiLockActive != 0,
            AntiLockSetting = (int)page.mAntiLockSetting,
            TractionControlSetting = (int)page.mTractionControlSetting,
            BrakeBias = page.mBrakeBias,

            CrashState = page.mCrashState,
            AeroDamage = page.mAeroDamage,
            EngineDamage = page.mEngineDamage,
        };

        // Motion arrays
        Array.Copy(page.mOrientation, clean.Orientation, 3);
        Array.Copy(page.mAngularVelocity, clean.AngularVelocity, 3);
        Array.Copy(page.mLocalVelocity, clean.LocalVelocity, 3);
        Array.Copy(page.mWorldVelocity, clean.WorldVelocity, 3);
        Array.Copy(page.mLocalAcceleration, clean.LocalAcceleration, 3);
        Array.Copy(page.mWorldAcceleration, clean.WorldAcceleration, 3);

        // Tyres
        Array.Copy(page.mTyreTempLeft, clean.TyreTempLeft, 4);
        Array.Copy(page.mTyreTempCenter, clean.TyreTempCenter, 4);
        Array.Copy(page.mTyreTempRight, clean.TyreTempRight, 4);
        Array.Copy(page.mAirPressure, clean.AirPressure, 4);
        Array.Copy(page.mTyreWear, clean.TyreWear, 4);
        Array.Copy(page.mTyreRPS, clean.TyreRPS, 4);
        Array.Copy(page.mBrakeTempCelsius, clean.BrakeTempCelsius, 4);

        // Platform
        Array.Copy(page.mSuspensionTravel, clean.SuspensionTravel, 4);
        Array.Copy(page.mSuspensionVelocity, clean.SuspensionVelocity, 4);
        Array.Copy(page.mRideHeight, clean.RideHeight, 4);

        // Viewed participant
        int viewed = (int)page.mViewedParticipantIndex;
        clean.Viewed.Index = viewed;
        var raw = page.mParticipantInfo[viewed];
        clean.Viewed.WorldPosition = raw.mWorldPosition;
        clean.Viewed.CurrentLapDistance = raw.mCurrentLapDistance;
        clean.Viewed.LapsCompleted = raw.mLapsCompleted;
        clean.Viewed.CurrentLap = raw.mCurrentLap;
        clean.Viewed.CurrentSector = raw.mCurrentSector;

        return clean;
    }

    // ---------- Main ----------
    static void Main()
    {
        var mem = new MemoryParser("$pcars2$");
        var options = new JsonSerializerOptions
        {
            WriteIndented = false,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
        };

        const int samplePeriodMs = 50; // 20 Hz

        Stream? currentFileStream = null;
        StreamWriter? currentWriter = null;
        string? currentTrackKey = null;

        AppDomain.CurrentDomain.ProcessExit += (s, e) => currentWriter?.Dispose();
        Console.CancelKeyPress += (s, e) =>
        {
            currentWriter?.Dispose();
            e.Cancel = false;
        };

        while (true)
        {
            try
            {
                var page = mem.GetPage();
                var timestamp = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

                string? trackKey = GetTrackKey(page);

                if (trackKey != null)
                {
                    if (trackKey != currentTrackKey)
                    {
                        currentWriter?.Dispose();
                        currentWriter = null;
                        currentFileStream?.Dispose();
                        currentFileStream = null;

                        string fileName = $"telemetry_{SanitizeFileName(trackKey)}.ndjson.gz";
                        currentFileStream = new FileStream(fileName, FileMode.Append, FileAccess.Write, FileShare.None);
                        var gzipStream = new GZipStream(currentFileStream, CompressionLevel.Optimal);
                        currentWriter = new StreamWriter(gzipStream);
                        currentTrackKey = trackKey;
                        Console.WriteLine($"Track changed -> writing to {fileName}");
                    }

                    if (currentWriter != null)
                    {
                        var cleanFrame = MapToCleanFrame(page, timestamp);
                        currentWriter.WriteLine(JsonSerializer.Serialize(cleanFrame, options));
                        if (timestamp % 500 == 0)
                            currentWriter.Flush();
                    }
                }
                else
                {
                    if (currentWriter != null)
                    {
                        currentWriter.Dispose();
                        currentWriter = null;
                        currentFileStream?.Dispose();
                        currentFileStream = null;
                        currentTrackKey = null;
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Error: {ex.Message}");
            }

            Thread.Sleep(samplePeriodMs);
        }
    }
}
