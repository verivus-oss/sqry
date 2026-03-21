-- ============================================================================
-- Neovim Plugin: CodeNavigator
-- A realistic Neovim plugin demonstrating common Lua patterns
-- ============================================================================
--
-- GROUND TRUTH ANNOTATIONS:
--
-- EXPORTS (18 total):
-- 1. M::setup (function, line 45)
-- 2. M::navigate_to_definition (function, line 97)
-- 3. M::find_references (function, line 132)
-- 4. M::show_hover (function, line 167)
-- 5. M::list_diagnostics (function, line 198)
-- 6. M::format_buffer (function, line 229)
-- 7. Config::new (method, colon, line 263)
-- 8. Config::merge (method, colon, line 278)
-- 9. Config::validate (method, colon, line 294)
-- 10. Config::get (method, colon, line 316)
-- 11. Buffer::new (method, colon, line 342)
-- 12. Buffer::get_content (method, colon, line 356)
-- 13. Buffer::set_content (method, colon, line 368)
-- 14. Buffer::save (method, colon, line 381)
-- 15. Window::new (method, colon, line 407)
-- 16. Window::focus (method, colon, line 421)
-- 17. Window::close (method, colon, line 433)
-- 18. Window::resize (method, colon, line 445)
--
-- CALLS (42 total):
-- 1. vim.api.nvim_create_user_command (line 72)
-- 2. vim.api.nvim_set_keymap (line 73)
-- 3. vim.api.nvim_set_keymap (line 74)
-- 4. vim.api.nvim_set_keymap (line 75)
-- 5. vim.api.nvim_create_augroup (line 79)
-- 6. vim.api.nvim_create_autocmd (line 80)
-- 7. config:validate (line 86, colon)
-- 8. vim.notify (line 90)
-- 9. Config:new (line 104, colon)
-- 10. config:merge (line 105, colon)
-- 11. vim.lsp.buf.definition (line 109)
-- 12. vim.api.nvim_win_set_cursor (line 113)
-- 13. Buffer:new (line 116, colon)
-- 14. buffer:get_content (line 117, colon)
-- 15. vim.notify (line 120)
-- 16. vim.lsp.util.show_line_diagnostics (line 125)
-- 17. Config:new (line 139, colon)
-- 18. config:merge (line 140, colon)
-- 19. vim.lsp.buf.references (line 144)
-- 20. Window:new (line 147, colon)
-- 21. window:focus (line 148, colon)
-- 22. vim.api.nvim_buf_set_lines (line 152)
-- 23. window:resize (line 156)
-- 24. vim.notify (line 160)
-- 25. Config:new (line 174, colon)
-- 26. config:merge (line 175, colon)
-- 27. vim.lsp.buf.hover (line 179)
-- 28. Window:new (line 182, colon)
-- 29. window:focus (line 183, colon)
-- 30. window:close (line 187)
-- 31. vim.notify (line 191)
-- 32. vim.diagnostic.get (line 205)
-- 33. Window:new (line 209, colon)
-- 34. window:focus (line 210, colon)
-- 35. vim.api.nvim_buf_set_lines (line 214)
-- 36. window:resize (line 218)
-- 37. vim.notify (line 222)
-- 38. Buffer:new (line 236, colon)
-- 39. buffer:get_content (line 237, colon)
-- 40. vim.lsp.buf.format (line 241)
-- 41. buffer:set_content (line 245)
-- 42. buffer:save (line 246)
--
-- ============================================================================

local M = {}

-- Module-level configuration
function M.setup(opts)
  opts = opts or {}

  -- Default configuration
  local config = {
    enable_diagnostics = true,
    enable_hover = true,
    enable_references = true,
    keymaps = {
      goto_definition = '<leader>gd',
      find_references = '<leader>gr',
      show_hover = '<leader>K',
    },
    window = {
      width = 80,
      height = 20,
      border = 'rounded',
    },
  }

  -- Merge user options
  config = vim.tbl_deep_extend('force', config, opts)

  -- Store configuration globally
  vim.g.code_navigator_config = config

  -- Register user commands
  vim.api.nvim_create_user_command('CodeNavDefinition', M.navigate_to_definition, {})
  vim.api.nvim_set_keymap('n', config.keymaps.goto_definition, '<cmd>CodeNavDefinition<CR>', { noremap = true, silent = true })
  vim.api.nvim_set_keymap('n', config.keymaps.find_references, '<cmd>lua require("code_navigator").find_references()<CR>', { noremap = true, silent = true })
  vim.api.nvim_set_keymap('n', config.keymaps.show_hover, '<cmd>lua require("code_navigator").show_hover()<CR>', { noremap = true, silent = true })

  -- Setup autocmds for diagnostics
  local group = vim.api.nvim_create_augroup('CodeNavigator', { clear = true })
  vim.api.nvim_create_autocmd('DiagnosticChanged', {
    group = group,
    callback = function()
      if config.enable_diagnostics then
        -- Update diagnostic display
        config:validate()
      end
    end,
  })

  -- Notify user
  vim.notify('CodeNavigator: Setup complete', vim.log.levels.INFO)
end

-- Navigate to definition of symbol under cursor
function M.navigate_to_definition()
  local bufnr = vim.api.nvim_get_current_buf()
  local cursor = vim.api.nvim_win_get_cursor(0)
  local row, col = cursor[1], cursor[2]

  -- Get configuration
  local config = Config:new(vim.g.code_navigator_config)
  config:merge({ buffer = bufnr, position = { row, col } })

  -- Request definition from LSP
  vim.lsp.buf.definition({
    on_list = function(items)
      if #items > 0 then
        vim.api.nvim_win_set_cursor(0, { items[1].lnum, items[1].col - 1 })

        -- Log to buffer
        local buffer = Buffer:new(bufnr)
        local content = buffer:get_content()

        -- Notify success
        vim.notify('Navigated to definition', vim.log.levels.INFO)

        -- Show diagnostics for current line
        vim.lsp.util.show_line_diagnostics()
      end
    end,
  })
end

-- Find all references to symbol under cursor
function M.find_references()
  local bufnr = vim.api.nvim_get_current_buf()
  local cursor = vim.api.nvim_win_get_cursor(0)
  local row, col = cursor[1], cursor[2]

  -- Get configuration
  local config = Config:new(vim.g.code_navigator_config)
  config:merge({ buffer = bufnr, position = { row, col } })

  -- Request references from LSP
  vim.lsp.buf.references(nil, {
    on_list = function(items)
      local win = Window:new(vim.g.code_navigator_config.window)
      win:focus()

      -- Display references in floating window
      local lines = {}
      vim.api.nvim_buf_set_lines(win.bufnr, 0, -1, false, lines)

      -- Resize window based on content
      win:resize(#lines)

      -- Notify user
      vim.notify(string.format('Found %d references', #items), vim.log.levels.INFO)
    end,
  })
end

-- Show hover information for symbol under cursor
function M.show_hover()
  local bufnr = vim.api.nvim_get_current_buf()
  local cursor = vim.api.nvim_win_get_cursor(0)
  local row, col = cursor[1], cursor[2]

  -- Get configuration
  local config = Config:new(vim.g.code_navigator_config)
  config:merge({ buffer = bufnr, position = { row, col } })

  -- Request hover from LSP
  vim.lsp.buf.hover({
    on_list = function(items)
      local win = Window:new(vim.g.code_navigator_config.window)
      win:focus()

      -- Close after timeout
      vim.defer_fn(function()
        win:close()
      end, 3000)

      -- Notify user
      vim.notify('Showing hover information', vim.log.levels.INFO)
    end,
  })
end

-- List all diagnostics in current buffer
function M.list_diagnostics()
  local bufnr = vim.api.nvim_get_current_buf()

  -- Get diagnostics
  local diagnostics = vim.diagnostic.get(bufnr)

  if #diagnostics > 0 then
    -- Create window for diagnostics
    local win = Window:new(vim.g.code_navigator_config.window)
    win:focus()

    -- Format diagnostics
    local lines = {}
    vim.api.nvim_buf_set_lines(win.bufnr, 0, -1, false, lines)

    -- Resize window
    win:resize(#lines)

    -- Notify user
    vim.notify(string.format('Found %d diagnostics', #diagnostics), vim.log.levels.INFO)
  end
end

-- Format current buffer
function M.format_buffer()
  local bufnr = vim.api.nvim_get_current_buf()

  -- Get current content
  local buffer = Buffer:new(bufnr)
  local content = buffer:get_content()

  -- Format using LSP
  vim.lsp.buf.format({
    bufnr = bufnr,
    async = false,
  })

  -- Update content
  buffer:set_content(content)
  buffer:save()

  -- Notify user
  vim.notify('Buffer formatted', vim.log.levels.INFO)
end

-- ============================================================================
-- Configuration Class
-- ============================================================================

Config = {}
Config.__index = Config

-- Constructor for Config
function Config:new(opts)
  opts = opts or {}
  local instance = {
    data = opts,
    valid = false,
  }
  setmetatable(instance, Config)
  return instance
end

-- Merge configuration with new options
function Config:merge(new_opts)
  if not new_opts then
    return self
  end

  self.data = vim.tbl_deep_extend('force', self.data, new_opts)
  self.valid = false

  return self
end

-- Validate configuration
function Config:validate()
  if self.valid then
    return true
  end

  -- Validation logic
  local required_keys = { 'enable_diagnostics', 'enable_hover', 'enable_references' }
  for _, key in ipairs(required_keys) do
    if self.data[key] == nil then
      vim.notify(string.format('Missing required config key: %s', key), vim.log.levels.ERROR)
      return false
    end
  end

  self.valid = true
  return true
end

-- Get configuration value
function Config:get(key)
  return self.data[key]
end

-- ============================================================================
-- Buffer Class
-- ============================================================================

Buffer = {}
Buffer.__index = Buffer

-- Constructor for Buffer
function Buffer:new(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  local instance = {
    bufnr = bufnr,
    name = vim.api.nvim_buf_get_name(bufnr),
  }
  setmetatable(instance, Buffer)
  return instance
end

-- Get buffer content
function Buffer:get_content()
  local lines = vim.api.nvim_buf_get_lines(self.bufnr, 0, -1, false)
  return table.concat(lines, '\n')
end

-- Set buffer content
function Buffer:set_content(content)
  local lines = vim.split(content, '\n')
  vim.api.nvim_buf_set_lines(self.bufnr, 0, -1, false, lines)
  return self
end

-- Save buffer
function Buffer:save()
  vim.api.nvim_buf_call(self.bufnr, function()
    vim.cmd('write')
  end)
  return self
end

-- ============================================================================
-- Window Class
-- ============================================================================

Window = {}
Window.__index = Window

-- Constructor for Window
function Window:new(opts)
  opts = opts or {}
  local bufnr = vim.api.nvim_create_buf(false, true)
  local width = opts.width or 80
  local height = opts.height or 20
  local instance = {
    bufnr = bufnr,
    width = width,
    height = height,
  }
  setmetatable(instance, Window)
  return instance
end

-- Focus window
function Window:focus()
  vim.api.nvim_set_current_buf(self.bufnr)
  return self
end

-- Close window
function Window:close()
  vim.api.nvim_buf_delete(self.bufnr, { force = true })
  return self
end

-- Resize window
function Window:resize(new_height)
  if new_height then
    self.height = new_height
  end
  return self
end

-- ============================================================================
-- Utility Functions (internal, not exported)
-- ============================================================================

local function log_debug(message)
  if vim.g.code_navigator_config and vim.g.code_navigator_config.debug then
    vim.notify(message, vim.log.levels.DEBUG)
  end
end

local function get_cursor_word()
  return vim.fn.expand('<cword>')
end

local function is_lsp_available()
  return #vim.lsp.get_active_clients() > 0
end

-- Callback handlers
local function on_definition_found(items)
  log_debug('Definition found')
  return items[1]
end

local function on_references_found(items)
  log_debug(string.format('Found %d references', #items))
  return items
end

local function on_hover_shown(result)
  log_debug('Hover information displayed')
  return result
end

-- Chained method calls for testing
local function chain_example()
  local config = Config:new({ debug = true })
    :merge({ enable_diagnostics = false })
    :validate()

  local buffer = Buffer:new()
    :get_content()

  local window = Window:new({ width = 100, height = 30 })
    :focus()
    :resize(40)
    :close()
end

-- Bracket-style calls (common in event handlers)
local handlers = {
  definition = M.navigate_to_definition,
  references = M.find_references,
  hover = M.show_hover,
  diagnostics = M.list_diagnostics,
  format = M.format_buffer,
}

local function dispatch_handler(handler_name)
  if handlers[handler_name] then
    handlers[handler_name]()
  end
end

-- Higher-order function example
local function with_lsp_client(callback)
  if is_lsp_available() then
    callback()
  else
    vim.notify('No LSP client available', vim.log.levels.WARN)
  end
end

-- Usage example
local function safe_navigate()
  with_lsp_client(function()
    M.navigate_to_definition()
  end)
end

-- Anonymous function in table
local autocmd_callbacks = {
  on_save = function()
    M.format_buffer()
  end,
  on_change = function()
    M.list_diagnostics()
  end,
}

-- Conditional method calls
local function conditional_example(should_format)
  local buffer = Buffer:new()

  if should_format then
    M.format_buffer()
  else
    buffer:get_content()
  end
end

-- Module pattern with metatable
local MetaModule = {}
MetaModule.__index = MetaModule

function MetaModule:create()
  local instance = {}
  setmetatable(instance, MetaModule)
  return instance
end

function MetaModule:process()
  Config:new():validate()
end

-- Varargs and dynamic calls
local function call_method(obj, method_name, ...)
  return obj[method_name](obj, ...)
end

-- Callback registration pattern
local registered_callbacks = {}

local function register_callback(name, callback)
  registered_callbacks[name] = callback
end

local function trigger_callback(name, ...)
  if registered_callbacks[name] then
    registered_callbacks[name](...)
  end
end

-- Setup default callbacks
register_callback('format', M.format_buffer)
register_callback('diagnostics', M.list_diagnostics)

-- Table constructor with method references
local actions = {
  gd = M.navigate_to_definition,
  gr = M.find_references,
  K = M.show_hover,

  -- Nested table
  advanced = {
    format = M.format_buffer,
    diagnostics = M.list_diagnostics,
  },
}

-- Execute action by key
local function execute_action(key)
  if actions[key] then
    actions[key]()
  end
end

-- Complex chaining with conditional branches
local function complex_chain()
  local config = Config:new()

  if config:validate() then
    config:merge({ debug = true })
  end

  return config:get('debug')
end

return M
