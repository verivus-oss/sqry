using System;

namespace LocalVars
{
    class NoFalsePositives
    {
        // Type names should NOT be local variable refs
        void TypeNames()
        {
            int x = 5;
            Console.WriteLine(x);
        }

        // Field access after dot should NOT be local refs
        void FieldAccess()
        {
            var s = "hello";
            int len = s.Length;
            Console.WriteLine(len);
        }

        // Method names should NOT be local refs
        void MethodCalls()
        {
            int x = 5;
            Console.WriteLine(x);
        }
    }
}
