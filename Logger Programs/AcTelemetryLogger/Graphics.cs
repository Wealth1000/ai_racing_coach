using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;

namespace AssettoCorsaSharedMemory
{
    public enum AC_PENALTY_TYPE
    {
        ACC_None = 0,
        ACC_DriveThrough_Cutting = 1,
        ACC_StopAndGo_10_Cutting = 2,
        ACC_StopAndGo_20_Cutting = 3,
        ACC_StopAndGo_30_Cutting = 4,
        ACC_Disqualified_Cutting = 5,
        ACC_RemoveBestLaptime_Cutting = 6,
        ACC_DriveThrough_PitSpeeding = 7,
        ACC_StopAndGo_10_PitSpeeding = 8,
        ACC_StopAndGo_20_PitSpeeding = 9,
        ACC_StopAndGo_30_PitSpeeding = 10,
        ACC_Disqualified_PitSpeeding = 11,
        ACC_RemoveBestLaptime_PitSpeeding = 12,
        ACC_Disqualified_IgnoredMandatoryPit = 13,
        ACC_PostRaceTime = 14,
        ACC_Disqualified_Trolling = 15,
        ACC_Disqualified_PitEntry = 16,
        ACC_Disqualified_PitExit = 17,
        ACC_Disqualified_Wrongway = 18,
        ACC_DriveThrough_IgnoredDriverStint = 19,
        ACC_Disqualified_IgnoredDriverStint = 20,
        ACC_Disqualified_ExceededDriverStintLimit = 21
    }
    public enum AC_FLAG_TYPE
    {
        AC_NO_FLAG = 0,
        AC_BLUE_FLAG = 1,
        AC_YELLOW_FLAG = 2,
        AC_BLACK_FLAG = 3,
        AC_WHITE_FLAG = 4,
        AC_CHECKERED_FLAG = 5,
        AC_PENALTY_FLAG = 6,
        AC_GREEN_FLAG = 7,
        AC_ORANGE_FLAG = 8
    }

    public enum AC_STATUS
    {
        AC_OFF = 0,
        AC_REPLAY = 1,
        AC_LIVE = 2,
        AC_PAUSE = 3
    }

    public enum AC_SESSION_TYPE
    {
        AC_UNKNOWN = -1,
        AC_PRACTICE = 0,
        AC_QUALIFY = 1,
        AC_RACE = 2,
        AC_HOTLAP = 3,
        AC_TIME_ATTACK = 4,
        AC_DRIFT = 5,
        AC_DRAG = 6
    }

    public enum AC_WHEELS_TYPE
    {
        AC_FrontLeft = 0,
        AC_FrontRight = 1,
        AC_RearLeft = 2,
        AC_RearRight = 3
    }

    public enum AC_TRACK_GRIP_STATUS
    {
        AC_GREEN = 0,
        AC_FAST = 1,
        AC_OPTIMUM = 2,
        AC_GREASY = 3,
        AC_DEMP = 4,
        AC_WET = 5,
        AC_FLOODED = 6
    }

    public enum AC_RAIN_INTENSITY
    {
        AC_NO_RAIN = 0,
        AC_DRIZZLE = 1,
        AC_LIGHT_RAIN = 2,
        AC_MEDIUM_RAIN = 3,
        AC_HEAVY_RAIN = 4,
        AC_THUNDERSTORM = 5
    }

    public class GraphicsEventArgs : EventArgs
    {
        public GraphicsEventArgs (Graphics graphics)
        {
            this.Graphics = graphics;
        }

        public Graphics Graphics { get; private set; }
    }

    [StructLayout (LayoutKind.Sequential, Pack = 4, CharSet = CharSet.Unicode)]
    [Serializable]
    public struct Graphics
    {
        public int PacketId;
        public AC_STATUS Status;
        public AC_SESSION_TYPE Session;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 15)]
        public String CurrentTime;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 15)]
        public String LastTime;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 15)]
        public String BestTime;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 15)]
        public String Split;
        public int CompletedLaps;
        public int Position;
        public int iCurrentTime;
        public int iLastTime;
        public int iBestTime;
        public float SessionTimeLeft;
        public float DistanceTraveled;
        public int IsInPit;
        public int CurrentSectorIndex;
        public int LastSectorTime;
        public int NumberOfLaps;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public String TyreCompound;

        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float ReplayTimeMultiplier;
        public float NormalizedCarPosition;    // 248

        // ---------------------------------------------------------------------------
        // AC's layout from here on, and it is NOT ACC's.
        //
        // The struct this file was derived from had, at offset 252:
        //     int ActiveCars; float[180] CarCoordinates; int[60] CarID; int PlayerCarID;
        // a 964-byte multi-car block that AC 1.x has never published. AC puts the
        // player's own carCoordinates[3] right here instead, so every field from 252
        // onward was being read from the wrong place -- which is why the self-test saw
        // "ActiveCars @252 = 1132052166": that int is the float 249.745, the car's X
        // coordinate at Red Bull Ring.
        //
        // Verified three ways against this exact install:
        //   - apps/python/TelemetryBridge/sim_info.py (Rombik's, version-matched)
        //   - extension/internal/python/lib/sim_info.py (shipped inside CSP) -- identical
        //   - SharedMemoryACS/ConsoleApplication1/SharedFileOut.h (Kunos' own sample),
        //     which likewise has carCoordinates immediately after normalizedCarPosition
        //     with no activeCars in between
        // and against the live page probe: last non-zero byte 283, exactly surfaceGrip's
        // last byte under this layout.
        //
        // windSpeed/windDirection are the final two fields; SPageFileGraphic ends at 296
        // bytes. Everything ACC appends after them (setup-menu and display indexes, the
        // TC/ABS readouts, fuelXLap, lights, driver stints, delta and estimated lap
        // times, trackStatus, the global flags, the MFD block, trackGripStatus, rain
        // intensity, tyre sets) does not exist in AC and is deliberately absent.
        // ---------------------------------------------------------------------------
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 3)]
        public float[] CarCoordinates;         // 252, 256, 260
        public float PenaltyTime;              // 264
        public AC_FLAG_TYPE Flag;              // 268
        public int IdealLineOn;                // 272

        // since 1.5
        public int IsInPitLane;                // 276
        public float SurfaceGrip;              // 280

        // since 1.13
        public int MandatoryPitDone;           // 284

        public float WindSpeed;                // 288
        public float WindDirection;            // 292  -> 296, end of page
    }
}
