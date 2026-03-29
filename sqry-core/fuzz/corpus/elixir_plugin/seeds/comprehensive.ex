defmodule Math do
  def add(a, b), do: a + b

  def add(a, b, c) do
    a + b + c
  end

  defp private_func(x) when is_integer(x) do
    x * 2
  end

  defmacro double(x) do
    quote do
      unquote(x) * 2
    end
  end
end
