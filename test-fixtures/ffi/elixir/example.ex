defmodule Example do
  @moduledoc """
  Example Elixir module demonstrating NIF (Native Implemented Function) loading.

  This module shows the typical pattern for using Erlang NIFs in Elixir:
  1. @on_load attribute to auto-initialize
  2. :erlang.load_nif/2 call to load the native library
  3. Stub functions that return :erlang.nif_error/1
  """

  @on_load :load_nifs

  @doc """
  Callback function invoked when the module is loaded.
  Loads the native library containing the NIF implementations.
  """
  def load_nifs do
    # Load the NIF library from priv directory
    # Path is relative to the compiled .beam file location
    :erlang.load_nif('./test-fixtures/ffi/elixir/example_nif', 0)
  end

  @doc """
  Computes a value using the native implementation.
  This is a stub - the real implementation is in C.
  """
  def compute(_x) do
    # This will be replaced by the native implementation
    # If the NIF fails to load, this error is raised
    :erlang.nif_error(:not_loaded)
  end

  @doc """
  Processes data using native code.
  Another stub function for demonstration.
  """
  def process_data(_data) do
    :erlang.nif_error(:not_loaded)
  end
end
