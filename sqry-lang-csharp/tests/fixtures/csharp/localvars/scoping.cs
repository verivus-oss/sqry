namespace LocalVars
{
    class ScopingTests
    {
        void ShadowedVar()
        {
            int x = 10;
            Console.WriteLine(x);
            {
                int x2 = 20;
                Console.WriteLine(x2);
            }
            Console.WriteLine(x);
        }

        void ForLoopVar()
        {
            for (int i = 0; i < 10; i++)
            {
                Console.WriteLine(i);
            }
        }

        void ForeachVar()
        {
            var items = new int[] { 1, 2, 3 };
            foreach (var item in items)
            {
                Console.WriteLine(item);
            }
        }

        void MultipleRefs()
        {
            int x = 1;
            int y = x + x;
            int z = x + y;
            Console.WriteLine(z);
        }
    }
}
