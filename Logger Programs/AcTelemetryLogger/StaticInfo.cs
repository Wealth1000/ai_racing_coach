using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;

namespace AssettoCorsaSharedMemory
{
    public class StaticInfoEventArgs : EventArgs
    {
        public StaticInfoEventArgs (StaticInfo staticInfo)
        {
            this.StaticInfo = staticInfo;
        }

        public StaticInfo StaticInfo { get; private set; }
    }

    [StructLayout (LayoutKind.Sequential, Pack = 4, CharSet = CharSet.Unicode)]
    [Serializable]
    public struct StaticInfo
    {
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 15)]
        public String SMVersion;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 15)]
        public String ACVersion;

        // session static info
        public int NumberOfSessions;
        public int NumCars;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public String CarModel;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public String Track;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public String PlayerName;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public String PlayerSurname;
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public String PlayerNick;

        public int SectorCount;

        // car static info
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float MaxTorque;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float MaxPower;
        public int MaxRpm;
        public float MaxFuel;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] SuspensionMaxTravel;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] TyreRadius;

        // since 1.5
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float MaxTurboBoost;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float Deprecated1; // AirTemp since 1.6 in physic
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float Deprecated2; // RoadTemp since 1.6 in physic
        public int PenaltiesEnabled;
        public float AidFuelRate;
        public float AidTireRate;
        public float AidMechanicalDamage;
        public float AidAllowTyreBlankets;
        public float AidStability;
        public int AidAutoClutch;
        public int AidAutoBlip;

        // since 1.7.1
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int HasDRS;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int HasERS;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int HasKERS;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float KersMaxJoules;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int EngineBrakeSettingsCount;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int ErsPowerControllerCount;

        // since 1.7.2
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float TrackSPlineLength;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public string TrackConfiguration;

        // since 1.10.2
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float ErsMaxJ;

        // since 1.13
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int IsTimedRace;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int HasExtraLap;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValTStr, SizeConst = 33)]
        public String CarSkin;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int ReversedGridPositions;
        public int PitWindowStart;
        public int PitWindowEnd;             // 680 -> 684

        /// <summary>
        /// Unconfirmed for AC 1.14. Both sim_info.py copies in this install end at
        /// pitWindowEnd, so this may sit 4 bytes past what AC publishes and simply read
        /// 0 forever. It is kept because it is last: an unpublished tail field reads as
        /// a zeroed page and shifts nothing behind it. Use --dump-graphics to settle it --
        /// it prints static 664..704, so a real isOnline shows up as 1 in an online
        /// session.
        /// </summary>
        public int IsOnline;                 // 684 -> 688

        // DryTyresName @688 and WetTyresName @754 used to follow. Both are ACC-only:
        // the live probe read them as empty with the whole page zero from offset 621,
        // and neither sim_info.py in this install has them. Removed rather than logged
        // as permanently-blank strings.
    }
}
