defmodule Demo.Math do
  def add(a, b), do: a + b

  def add(a, b) when is_integer(a) and is_integer(b) do
    a + b
  end

  defp secret(x) do
    x * x
  end

  defmacro say(message) do
    quote do
      IO.puts(unquote(message))
    end
  end

  defstruct value: 0

  defmodule Helpers do
    def double(x), do: x * 2
  end
end
