namespace LocalVars
{
    class BasicVars
    {
        void SimpleVar()
        {
            int x = 10;
            int y = x + 1;
            Console.WriteLine(y);
        }

        void VarKeyword()
        {
            var count = 42;
            Console.WriteLine(count);
        }

        void MultipleVars()
        {
            int a = 1, b = 2;
            int c = a + b;
            Console.WriteLine(c);
        }

        void ParamRef(string name, int age)
        {
            var result = name;
            Console.WriteLine(result);
            Console.WriteLine(age);
        }
    }
}
