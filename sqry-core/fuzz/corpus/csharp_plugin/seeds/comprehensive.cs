using System;

namespace MyApp
{
    public class Calculator
    {
        private int value;

        public async Task<int> AddAsync(int a, int b)
        {
            return await Task.FromResult(a + b);
        }

        public virtual int Multiply(int a, int b) => a * b;
    }

    public interface ILogger
    {
        void Log(string message);
    }
}
