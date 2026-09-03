using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;

namespace AssettoCorsaSharedMemory
{
    public class PhysicsEventArgs : EventArgs
    {
        public PhysicsEventArgs (Physics physics)
        {
            this.Physics = physics;
        }

        public string Gear(int gear)
        {
            if (gear > 1) return (gear - 1).ToString();
            if (gear == 0) return "R";
            else return "N";
        }

        public Physics Physics { get; private set; }
    }

    [StructLayout (LayoutKind.Sequential)]
    public struct Coordinates
    {
        public float X;
        public float Y;
        public float Z;
    }

    [StructLayout (LayoutKind.Sequential, Pack = 4, CharSet = CharSet.Unicode)]
    [Serializable]
    public struct Physics
    {
        public int PacketId;
        public float Gas;
        public float Brake;
        public float Fuel;
        public int Gear;
        public int Rpms;
        public float SteerAngle;
        public float SpeedKmh;

        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 3)]
        public float[] Velocity;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 3)]
        public float[] AccG;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] WheelSlip;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] WheelLoad;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] WheelPressure;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] WheelAngularSpeed;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] TyreWear;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] TyreDirtyLevel;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] TyreCoreTemp;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] CamberRad;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] SuspensionTravel;

        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float Drs;
        public float TC;
        public float Heading;
        public float Pitch;
        public float Roll;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float CgHeight;

        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 5)]
        public float[] CarDamage;

        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int NumberOfTyresOut;
        public int PitLimiterOn;
        public float Abs;

        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float KersCharge;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float KersInput;
        public int AutoShifterOn;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 2)]
        public float[] RideHeight;

        // since 1.5
        public float TurboBoost;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float Ballast;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float AirDensity;

        // since 1.6
        public float AirTemp;
        public float RoadTemp;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 3)]
        public float[] LocalAngularVelocity;
        public float FinalFF;

        // since 1.7
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float PerformanceMeter;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int EngineBrake;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int ErsRecoveryLevel;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int ErsPowerLevel;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int ErsHeatCharging;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int ErsisCharging;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public float KersCurrentKJ;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int DrsAvailable;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        public int DrsEnabled;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] BrakeTemp;

        // since 1.10
        public float Clutch;

        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] TyreTempI;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] TyreTempM;
        /// <summary>
        /// Not used in ACC
        /// </summary>
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public float[] TyreTempO;

        // since 1.10.2
        public int IsAIControlled;

        // since 1.11
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public Coordinates[] TyreContactPoint;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public Coordinates[] TyreContactNormal;
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 4)]
        public Coordinates[] TyreContactHeading;
        public float BrakeBias;

        // since 1.12 -- and the LAST field AC publishes. localVelocity occupies
        // 568..579, so SPageFilePhysics ends at 580 bytes.
        [MarshalAs (UnmanagedType.ByValArray, SizeConst = 3)]
        public float[] LocalVelocity;

        // Everything ACC appends after localVelocity is deliberately absent here:
        // p2pActivation/p2pStatus, currentMaxRpm, mz/fx/fy, slipRatio/slipAngle,
        // tcInAction/absInAction, suspensionDamage, tyreTemp, waterTemp, brake
        // pressure/compound/padLife/discLife, ignition/starter/isEngineRunning and the
        // kerb/slip/g/abs vibration block.
        //
        // AC 1.14.1 (shared memory 1.7) does not publish any of it -- confirmed three
        // ways: the sim_info.py bundled with this install ends at localVelocity, so does
        // the copy shipped inside Custom Shaders Patch, and the live page probe reported
        // "all zero from offset 580 of 800". Keeping the fields meant marshalling 220
        // bytes of never-written page as zeros and writing them out as if they were
        // readings.
    }
}
