using System;

namespace LocalVars
{
    class AdvancedTests
    {
        void LambdaCapture()
        {
            int x = 10;
            Func<int> fn = () => x;
            Console.WriteLine(fn());
        }

        void TryCatchVar()
        {
            try
            {
                int x = 42;
                Console.WriteLine(x);
            }
            catch (Exception ex)
            {
                Console.WriteLine(ex);
            }
        }

        void SwitchVar()
        {
            int x = 3;
            switch (x)
            {
                case 1:
                    int y = 10;
                    Console.WriteLine(y);
                    break;
                case 2:
                    int z = 20;
                    Console.WriteLine(z);
                    break;
            }
        }

        void UsingVar()
        {
            using (var stream = new System.IO.MemoryStream())
            {
                Console.WriteLine(stream.Length);
            }
        }
    }
}
