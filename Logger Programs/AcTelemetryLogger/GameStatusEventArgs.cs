using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;

namespace AssettoCorsaSharedMemory
{
    public class GameStatusEventArgs : EventArgs
    {
        public AC_STATUS GameStatus {get; private set;}

        public GameStatusEventArgs(AC_STATUS status)
        {
            GameStatus = status;
        }
    }
    public class PitStatusEventArgs : EventArgs
    {
        public int PitStatus { get; private set; }

        public PitStatusEventArgs(int status)
        {
            PitStatus = status;
        }
    }
    public class SessionTypeEventArgs : EventArgs
    {
        public AC_SESSION_TYPE SessionType { get; private set; }

        public SessionTypeEventArgs(AC_SESSION_TYPE status)
        {
            SessionType = status;
        }
    }
}
