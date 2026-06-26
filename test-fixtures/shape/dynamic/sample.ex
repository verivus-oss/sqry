# Hand-written Elixir control-flow sample for shape descriptor coverage.

defmodule Classifier do
  def classify(value, label \\ "n/a", rest \\ []) do
    result =
      if value > 100 do
        1
      else
        if value > 10, do: 2, else: 3
      end

    bucket =
      case value do
        0 -> :zero
        n when n in 1..9 -> :small
        _ -> :large
      end

    cond do
      result == 1 -> helper(result)
      result == 2 -> helper(result + 1)
      true -> :ok
    end

    mapped = Enum.map(rest, fn x -> x * 2 end)

    filtered = for x <- rest, x > 0, do: x

    with {:ok, parsed} <- parse(value),
         true <- parsed > 0 do
      helper(parsed)
    else
      _ -> :error
    end

    try do
      if value < 0, do: raise("bad")
      helper(value)
    rescue
      e in RuntimeError -> :rescued
    catch
      :thrown -> :caught
    after
      cleanup()
    end

    {bucket, mapped, filtered, label}
  end

  def helper(x), do: x + 1

  defp parse(v), do: {:ok, v}

  defp cleanup, do: :ok
end
