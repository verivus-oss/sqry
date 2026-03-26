defmodule Demo.Relations do
  alias Demo.Math.Helpers
  alias Demo.Deep.Module, as: AliasModule

  import Demo.Math
  import Demo.Math.Helpers, only: [double: 1]

  require Logger
  use GenServer

  def handle_call(state), do: state
end
