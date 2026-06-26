// Hand-written control-flow sample for the C# body-shape descriptor coverage
// test. Exercises branch, loop, switch, try/catch/throw, return, break/continue,
// a call, and a lambda so the canonical CfBucket histogram is non-empty.

using System;

namespace Shapes
{
    public class Classifier
    {
        public int Classify(int n, string label = "default")
        {
            int total = 0;
            Func<int, int> twice = x => x * 2;

            if (n > 0)
            {
                total = twice(n);
            }
            else
            {
                total = 0;
            }

            for (int i = 0; i < n; i++)
            {
                if (i == 3)
                {
                    continue;
                }
                total += i;
            }

            while (total < 100)
            {
                total += 1;
                if (total == 50)
                {
                    break;
                }
            }

            switch (n)
            {
                case 0:
                    Console.WriteLine(total);
                    break;
                default:
                    Console.WriteLine(0);
                    break;
            }

            try
            {
                if (total < 0)
                {
                    throw new InvalidOperationException("negative");
                }
            }
            catch (Exception)
            {
                total = -1;
            }

            return total;
        }
    }
}
