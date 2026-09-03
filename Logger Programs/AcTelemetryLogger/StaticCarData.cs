using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace AssettoCorsaSharedMemory
{
    public class CarData
    {
        public float BrakeBias;
        public int? ShiftRpm = null;
        public int? MaxEngineMap = null;
        public int? MaxABS = null;
        public int? MaxTC1 = null;
        public int? MaxTC2 = null;
    }

    public class StaticCarData
    {
        public static Dictionary<string, CarData> Cars = new Dictionary<string, CarData>()
        {
            { "amr_v8_vantage_gt3", new CarData(){ BrakeBias = -0.07f, MaxEngineMap = 8 } },
            { "amr_v12_vantage_gt3", new CarData(){ BrakeBias = -0.07f } },
            { "audi_r8_lms", new CarData(){ BrakeBias = -0.14f } },
            { "bentley_continental_gt3_2016", new CarData(){ BrakeBias = -0.07f } },
            { "bentley_continental_gt3_2018", new CarData(){ BrakeBias = -0.07f, MaxEngineMap = 8 } },
            { "bmw_m6_gt3", new CarData(){ BrakeBias = -0.15f, MaxEngineMap = 9 } },
            { "jaguar_g3", new CarData(){ BrakeBias = -0.07f } },
            { "ferrari_488_gt3", new CarData(){ BrakeBias = -0.17f } },
            { "ferrari_488_gt3_evo", new CarData(){ BrakeBias = -0.17f, MaxEngineMap = 12 } },
            { "honda_nsx_gt3", new CarData(){ BrakeBias = -0.14f } },
            { "lamborghini_gallardo_rex", new CarData(){ BrakeBias = -0.14f } },
            { "lamborghini_huracan_gt3", new CarData(){ BrakeBias = -0.14f } },
            { "lamborghini_huracan_st", new CarData(){ BrakeBias = -0.14f } },
            { "lexus_rc_f_gt3", new CarData(){ BrakeBias = -0.14f, MaxEngineMap = 8 } },
            { "mclaren_650s_gt3", new CarData(){ BrakeBias = -0.17f } },
            { "mercedes_amg_gt3", new CarData(){ BrakeBias = -0.14f } },
            { "mercedes_amg_gt3_evo", new CarData(){ BrakeBias = -0.14f, MaxEngineMap = 3 } },
            { "nissan_gt_r_gt3_2017", new CarData(){ BrakeBias = -0.15f } },
            { "nissan_gt_r_gt3_2018", new CarData(){ BrakeBias = -0.15f, MaxEngineMap = 4 } },
            { "porsche_991_gt3_r", new CarData(){ BrakeBias = -0.21f } },
            { "porsche_991ii_gt3_cup", new CarData(){ BrakeBias = -0.15f } },
            { "audi_r8_lms_evo", new CarData(){ BrakeBias = -0.14f, MaxEngineMap = 8 } },
            { "honda_nsx_gt3_evo", new CarData(){ BrakeBias = -0.14f, MaxEngineMap = 8 } },
            { "lamborghini_huracan_gt3_evo", new CarData(){ BrakeBias = -0.14f, MaxEngineMap = 8 } },
            { "mclaren_720s_gt3", new CarData(){ BrakeBias = -0.17f, MaxEngineMap = 13 } },
            { "porsche_991ii_gt3_r", new CarData(){ BrakeBias = -0.21f, MaxEngineMap = 8 } },
            { "alpine_a110_gt4", new CarData(){ BrakeBias = -0.15f } },
            { "amr_v8_vantage_gt4", new CarData(){ BrakeBias = -0.20f } },
            { "audi_r8_gt4", new CarData(){ BrakeBias = -0.15f } },
            { "bmw_m4_gt4", new CarData(){ BrakeBias = -0.22f } },
            { "chevrolet_camaro_gt4r", new CarData(){ BrakeBias = -0.18f } },
            { "ginetta_g55_gt4", new CarData(){ BrakeBias = -0.18f } },
            { "ktm_xbow_gt4", new CarData(){ BrakeBias = -0.20f } },
            { "maserati_mc_gt4", new CarData(){ BrakeBias = -0.15f } },
            { "mclaren_570s_gt4", new CarData(){ BrakeBias = -0.09f } },
            { "mercedes_amg_gt4", new CarData(){ BrakeBias = -0.20f } },
            { "porsche_718_cayman_gt4_mr", new CarData(){ BrakeBias = -0.20f } }
        };
    }
}
