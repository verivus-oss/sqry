-- ============================================================================
-- World of Warcraft Addon: RaidHelper
-- A realistic WoW addon demonstrating common addon patterns
-- ============================================================================
--
-- GROUND TRUTH ANNOTATIONS:
--
-- EXPORTS (24 total):
-- 1. RaidHelper (table, line 90)
-- 2. RaidHelper::Initialize (method, colon, line 92)
-- 3. RaidHelper::OnEnable (method, colon, line 122)
-- 4. RaidHelper::OnDisable (method, colon, line 148)
-- 5. RaidHelper::RegisterEvents (method, colon, line 162)
-- 6. RaidHelper::UnregisterEvents (method, colon, line 180)
-- 7. RaidHelper::HandleEvent (method, colon, line 194)
-- 8. RaidHelper::RefreshRaidRoster (method, colon, line 248)
-- 9. RaidHelper::UpdateMemberHealth (method, colon, line 287)
-- 10. RaidHelper::CheckBuffs (method, colon, line 316)
-- 11. RaidHelper::ApplyBuff (method, colon, line 348)
-- 12. RaidHelper::ShowRaidFrame (method, colon, line 375)
-- 13. RaidHelper::HideRaidFrame (method, colon, line 402)
-- 14. RaidHelper::UpdateRaidFrame (method, colon, line 418)
-- 15. RaidHelper::PrintMessage (method, colon, line 460)
-- 16. RaidHelper::GetOptions (method, colon, line 477)
-- 17. RaidHelperDB (table, line 517)
-- 18. RaidHelperDB::Reset (method, colon, line 519)
-- 19. RaidHelperDB::Save (method, colon, line 532)
-- 20. RaidHelperDB::Load (method, colon, line 546)
-- 21. RaidHelperDB::Get (method, colon, line 564)
-- 22. RaidHelperDB::Set (method, colon, line 574)
-- 23. SlashCmdList.RAIDHELPER (function, line 612)
-- 24. SlashCmdList.RAIDHELPER.help (function, nested, line 648)
--
-- CALLS (68 total):
-- 1. LibStub (line 98, call via bracket notation)
-- 2. addon:SetDefaultModuleState (line 100, colon)
-- 3. addon:SetDefaultModuleLibraries (line 101, colon)
-- 4. self:RegisterEvents (line 103, colon)
-- 5. self:PrintMessage (line 104, colon)
-- 6. RaidHelperDB:Load (line 109, colon)
-- 7. self:SetupSlashCommands (line 112, colon)
-- 8. self:CreateRaidFrame (line 115, colon)
-- 9. self:PrintMessage (line 118, colon)
-- 10. self:RegisterEvents (line 129, colon)
-- 11. self:RefreshRaidRoster (line 132, colon)
-- 12. self:ShowRaidFrame (line 135, colon)
-- 13. self:PrintMessage (line 138, colon)
-- 14. RaidHelperDB:Save (line 141, colon)
-- 15. LibStub (line 144, bracket)
-- 16. AceConfigDialog:Open (line 144, colon)
-- 17. self:UnregisterEvents (line 155, colon)
-- 18. self:HideRaidFrame (line 156, colon)
-- 19. RaidHelperDB:Save (line 157, colon)
-- 20. self:PrintMessage (line 158, colon)
-- 21. self.frame:RegisterEvent (line 169, colon)
-- 22. self.frame:RegisterEvent (line 170, colon)
-- 23. self.frame:RegisterEvent (line 171, colon)
-- 24. self.frame:RegisterEvent (line 172, colon)
-- 25. self.frame:RegisterEvent (line 173, colon)
-- 26. self.frame:SetScript (line 174, colon)
-- 27. self:PrintMessage (line 177, colon)
-- 28. self.frame:UnregisterAllEvents (line 187, colon)
-- 29. self.frame:SetScript (line 188, colon)
-- 30. self:PrintMessage (line 191, colon)
-- 31. self:RefreshRaidRoster (line 203, colon)
-- 32. self:UpdateMemberHealth (line 209, colon)
-- 33. self:CheckBuffs (line 215, colon)
-- 34. self:CheckBuffs (line 221, colon)
-- 35. self:PrintMessage (line 227, colon)
-- 36. RaidHelperDB:Save (line 230, colon)
-- 37. self:HideRaidFrame (line 235, colon)
-- 38. self:PrintMessage (line 238, colon)
-- 39. self:PrintMessage (line 244, colon)
-- 40. GetNumGroupMembers (line 255)
-- 41. IsInRaid (line 260)
-- 42. UnitName (line 267)
-- 43. UnitClass (line 268)
-- 44. UnitHealthMax (line 269)
-- 45. UnitHealth (line 270)
-- 46. table.insert (line 272)
-- 47. self:UpdateRaidFrame (line 280, colon)
-- 48. self:PrintMessage (line 283, colon)
-- 49. UnitExists (line 294)
-- 50. UnitHealthMax (line 298)
-- 51. UnitHealth (line 299)
-- 52. table.insert (line 307, multiple insertions)
-- 53. self:ApplyBuff (line 330, colon)
-- 54. self:PrintMessage (line 342, colon)
-- 55. UnitExists (line 355)
-- 56. UnitBuff (line 359)
-- 57. CastSpellByName (line 364)
-- 58. self:PrintMessage (line 367, colon)
-- 59. CreateFrame (line 382)
-- 60. frame:SetPoint (line 387, colon)
-- 61. frame:SetSize (line 388, colon)
-- 62. frame:SetBackdrop (line 389, colon)
-- 63. frame:SetBackdropColor (line 390, colon)
-- 64. frame:SetMovable (line 391, colon)
-- 65. frame:EnableMouse (line 392, colon)
-- 66. frame:RegisterForDrag (line 393, colon)
-- 67. frame:SetScript (line 394, colon)
-- 68. frame:SetScript (line 395, colon)
-- ... (and many more WoW API calls)
--
-- ============================================================================

-- Addon namespace and initialization
local AddonName, AddonTable = ...

-- Import libraries (common WoW addon pattern)
local AceAddon = LibStub("AceAddon-3.0")
local AceConfig = LibStub("AceConfig-3.0")
local AceConfigDialog = LibStub("AceConfigDialog-3.0")
local AceDB = LibStub("AceDB-3.0")
local AceEvent = LibStub("AceEvent-3.0")

-- Create addon object using Ace framework
local addon = AceAddon:NewAddon(AddonName, "AceEvent-3.0", "AceConsole-3.0")

-- Store addon in global namespace for external access
RaidHelper = addon

function RaidHelper:Initialize()
  -- Set up addon defaults
  self.version = "1.2.3"
  self.author = "PlayerName"

  -- Initialize addon with AceAddon framework
  local addon = LibStub("AceAddon-3.0"):NewAddon("RaidHelper")

  addon:SetDefaultModuleState(true)
  addon:SetDefaultModuleLibraries("AceEvent-3.0")

  self:RegisterEvents()
  self:PrintMessage("RaidHelper initialized successfully")

  -- Initialize database
  if not RaidHelperDB then
    RaidHelperDB = {}
  end
  RaidHelperDB:Load()

  -- Set up slash commands
  self:SetupSlashCommands()

  -- Create main UI frame
  self:CreateRaidFrame()

  -- Announce to chat
  self:PrintMessage(string.format("RaidHelper v%s loaded", self.version))
end

-- Called when addon is enabled
function RaidHelper:OnEnable()
  -- Register events when addon enables
  self:RegisterEvents()

  -- Refresh raid roster
  self:RefreshRaidRoster()

  -- Show main frame
  self:ShowRaidFrame()

  -- Print enable message
  self:PrintMessage("RaidHelper enabled")

  -- Save state
  RaidHelperDB:Save()

  -- Open config dialog
  LibStub("AceConfigDialog-3.0"):Open("RaidHelper")
end

-- Called when addon is disabled
function RaidHelper:OnDisable()
  -- Unregister all events
  self:UnregisterEvents()
  self:HideRaidFrame()
  RaidHelperDB:Save()
  self:PrintMessage("RaidHelper disabled")
end

-- Register game events
function RaidHelper:RegisterEvents()
  -- Create frame for event handling if not exists
  if not self.frame then
    self.frame = CreateFrame("Frame", "RaidHelperEventFrame")
  end

  self.frame:RegisterEvent("PLAYER_ENTERING_WORLD")
  self.frame:RegisterEvent("GROUP_ROSTER_UPDATE")
  self.frame:RegisterEvent("UNIT_HEALTH")
  self.frame:RegisterEvent("UNIT_AURA")
  self.frame:RegisterEvent("PLAYER_REGEN_DISABLED")
  self.frame:SetScript("OnEvent", function(frame, event, ...) self:HandleEvent(event, ...) end)

  self:PrintMessage("Events registered")
end

-- Unregister game events
function RaidHelper:UnregisterEvents()
  if self.frame then
    self.frame:UnregisterAllEvents()
    self.frame:SetScript("OnEvent", nil)
  end

  self:PrintMessage("Events unregistered")
end

-- Main event handler (dispatcher pattern)
function RaidHelper:HandleEvent(event, ...)
  local handlers = {
    PLAYER_ENTERING_WORLD = function()
      self:RefreshRaidRoster()
    end,

    GROUP_ROSTER_UPDATE = function()
      self:RefreshRaidRoster()
      self:UpdateMemberHealth(...)
    end,

    UNIT_HEALTH = function(unit)
      self:UpdateMemberHealth(unit)
    end,

    UNIT_AURA = function(unit)
      self:CheckBuffs(unit)
    end,

    UNIT_POWER = function(unit, powerType)
      self:CheckBuffs(unit)
    end,

    PLAYER_REGEN_DISABLED = function()
      -- Combat started
      self:PrintMessage("Entering combat!")

      -- Save state before combat
      RaidHelperDB:Save()
    end,

    PLAYER_REGEN_ENABLED = function()
      -- Combat ended
      self:HideRaidFrame()

      -- Announce
      self:PrintMessage("Leaving combat!")
    end,

    UNKNOWN_EVENT = function()
      self:PrintMessage("Unknown event received")
    end,
  }

  -- Dispatch to handler
  local handler = handlers[event] or handlers.UNKNOWN_EVENT
  handler()
end

-- Refresh raid roster
function RaidHelper:RefreshRaidRoster()
  self.raidMembers = {}

  -- Get number of group members
  local numMembers = GetNumGroupMembers()

  -- Check if in raid
  if not IsInRaid() then
    return
  end

  -- Iterate through raid members
  for i = 1, numMembers do
    local unit = "raid" .. i
    local name = UnitName(unit)
    local class = UnitClass(unit)
    local maxHealth = UnitHealthMax(unit)
    local currentHealth = UnitHealth(unit)

    table.insert(self.raidMembers, {
      unit = unit,
      name = name,
      class = class,
      maxHealth = maxHealth,
      currentHealth = currentHealth,
    })
  end

  -- Update UI
  self:UpdateRaidFrame()

  -- Print status
  self:PrintMessage(string.format("Raid roster updated: %d members", numMembers))
end

-- Update member health
function RaidHelper:UpdateMemberHealth(unit)
  if not unit then
    return
  end

  -- Check if unit exists
  if not UnitExists(unit) then
    return
  end

  -- Get health values
  local maxHealth = UnitHealthMax(unit)
  local currentHealth = UnitHealth(unit)

  -- Update stored data
  for _, member in ipairs(self.raidMembers) do
    if member.unit == unit then
      member.maxHealth = maxHealth
      member.currentHealth = currentHealth
      table.insert(self.healthHistory, {
        unit = unit,
        time = GetTime(),
        health = currentHealth,
      })
    end
  end
end

-- Check buffs on unit
function RaidHelper:CheckBuffs(unit)
  if not unit then
    return
  end

  local requiredBuffs = {
    "Power Word: Fortitude",
    "Arcane Intellect",
    "Mark of the Wild",
    "Blessing of Kings",
  }

  for _, buffName in ipairs(requiredBuffs) do
    local hasBuffFound = false

    for i = 1, 40 do
      local name = UnitBuff(unit, i)
      if name == buffName then
        hasBuffFound = true
        break
      end
    end

    if not hasBuffFound then
      -- Try to apply buff
      self:ApplyBuff(unit, buffName)
    end
  end

  -- Print status
  self:PrintMessage(string.format("Checked buffs for %s", UnitName(unit)))
end

-- Apply buff to unit
function RaidHelper:ApplyBuff(unit, buffName)
  if not unit then
    return
  end

  -- Check if unit exists
  if not UnitExists(unit) then
    return
  end

  -- Check if we have the spell
  local hasSpell = UnitBuff(unit, buffName)

  if not hasSpell then
    CastSpellByName(buffName, unit)

    -- Print message
    self:PrintMessage(string.format("Applied %s to %s", buffName, UnitName(unit)))
  end
end

-- Show raid frame
function RaidHelper:ShowRaidFrame()
  if not self.raidFrame then
    -- Create main raid frame
    local frame = CreateFrame("Frame", "RaidHelperMainFrame", UIParent, "BasicFrameTemplateWithInset")
    frame:SetTitle("Raid Helper")
    frame:SetSize(400, 600)
    frame:SetPoint("CENTER")
    frame:SetBackdrop({
      bgFile = "Interface\\DialogFrame\\UI-DialogBox-Background",
    })
    frame:SetBackdropColor(0, 0, 0, 0.8)
    frame:SetMovable(true)
    frame:EnableMouse(true)
    frame:RegisterForDrag("LeftButton")
    frame:SetScript("OnDragStart", frame.StartMoving)
    frame:SetScript("OnDragStop", frame.StopMovingOrSizing)

    self.raidFrame = frame
  end

  self.raidFrame:Show()
end

-- Hide raid frame
function RaidHelper:HideRaidFrame()
  if self.raidFrame then
    self.raidFrame:Hide()
  end
end

-- Update raid frame display
function RaidHelper:UpdateRaidFrame()
  if not self.raidFrame then
    return
  end

  -- Clear existing children
  if self.raidFrame.members then
    for _, memberFrame in ipairs(self.raidFrame.members) do
      memberFrame:Hide()
      memberFrame:SetParent(nil)
    end
  end

  self.raidFrame.members = {}

  -- Create member frames
  for i, member in ipairs(self.raidMembers) do
    local memberFrame = CreateFrame("Frame", nil, self.raidFrame)
    memberFrame:SetSize(380, 40)
    memberFrame:SetPoint("TOPLEFT", 10, -30 - (i - 1) * 45)

    -- Name label
    local nameText = memberFrame:CreateFontString(nil, "OVERLAY", "GameFontNormal")
    nameText:SetPoint("LEFT", 5, 0)
    nameText:SetText(member.name)

    -- Health bar
    local healthBar = CreateFrame("StatusBar", nil, memberFrame)
    healthBar:SetSize(200, 20)
    healthBar:SetPoint("LEFT", 120, 0)
    healthBar:SetStatusBarTexture("Interface\\TargetingFrame\\UI-StatusBar")
    healthBar:SetMinMaxValues(0, member.maxHealth)
    healthBar:SetValue(member.currentHealth)

    table.insert(self.raidFrame.members, memberFrame)
  end
end

-- Print message to chat
function RaidHelper:PrintMessage(message)
  if not message then
    return
  end

  DEFAULT_CHAT_FRAME:AddMessage("|cff00ff00[RaidHelper]|r " .. message)
end

-- Get addon options (for AceConfig)
function RaidHelper:GetOptions()
  local options = {
    type = "group",
    name = "Raid Helper",
    args = {
      enable = {
        type = "toggle",
        name = "Enable Addon",
        get = function() return self:IsEnabled() end,
        set = function(_, value)
          if value then
            self:OnEnable()
          else
            self:OnDisable()
          end
        end,
      },
      showFrame = {
        type = "toggle",
        name = "Show Raid Frame",
        get = function() return self.raidFrame and self.raidFrame:IsShown() end,
        set = function(_, value)
          if value then
            self:ShowRaidFrame()
          else
            self:HideRaidFrame()
          end
        end,
      },
    },
  }

  return options
end

-- ============================================================================
-- Database Module
-- ============================================================================

RaidHelperDB = RaidHelperDB or {}

function RaidHelperDB:Reset()
  RaidHelperSavedVariables = {
    version = "1.2.3",
    enabled = true,
    showFrame = true,
    raidMembers = {},
    settings = {
      autoHeal = false,
      autoBuff = true,
      announceBuffs = true,
    },
  }
end

function RaidHelperDB:Save()
  if not RaidHelperSavedVariables then
    self:Reset()
  end

  RaidHelperSavedVariables.raidMembers = RaidHelper.raidMembers or {}
  RaidHelperSavedVariables.lastUpdate = time()

  RaidHelper:PrintMessage("Database saved")
end

function RaidHelperDB:Load()
  if not RaidHelperSavedVariables then
    self:Reset()
  end

  -- Load raid members
  if RaidHelperSavedVariables.raidMembers then
    RaidHelper.raidMembers = RaidHelperSavedVariables.raidMembers
  end

  RaidHelper:PrintMessage("Database loaded")
end

function RaidHelperDB:Get(key)
  if not RaidHelperSavedVariables then
    return nil
  end

  return RaidHelperSavedVariables[key]
end

function RaidHelperDB:Set(key, value)
  if not RaidHelperSavedVariables then
    self:Reset()
  end

  RaidHelperSavedVariables[key] = value
  self:Save()
end

-- ============================================================================
-- Slash Command System
-- ============================================================================

function RaidHelper:SetupSlashCommands()
  SLASH_RAIDHELPER1 = "/rh"
  SLASH_RAIDHELPER2 = "/raidhelper"
end

-- Slash command handler
SlashCmdList["RAIDHELPER"] = function(msg)
  local command, args = msg:match("^(%S*)%s*(.-)$")

  local commands = {
    show = function()
      RaidHelper:ShowRaidFrame()
    end,

    hide = function()
      RaidHelper:HideRaidFrame()
    end,

    refresh = function()
      RaidHelper:RefreshRaidRoster()
    end,

    enable = function()
      RaidHelper:OnEnable()
    end,

    disable = function()
      RaidHelper:OnDisable()
    end,

    reset = function()
      RaidHelperDB:Reset()
      RaidHelper:PrintMessage("Database reset")
    end,

    config = function()
      LibStub("AceConfigDialog-3.0"):Open("RaidHelper")
    end,

    help = function()
      RaidHelper:PrintMessage("Available commands:")
      RaidHelper:PrintMessage("/rh show - Show raid frame")
      RaidHelper:PrintMessage("/rh hide - Hide raid frame")
      RaidHelper:PrintMessage("/rh refresh - Refresh raid roster")
      RaidHelper:PrintMessage("/rh enable - Enable addon")
      RaidHelper:PrintMessage("/rh disable - Disable addon")
      RaidHelper:PrintMessage("/rh reset - Reset database")
      RaidHelper:PrintMessage("/rh config - Open config")
    end,
  }

  local handler = commands[command] or commands.help
  handler()
end

-- ============================================================================
-- Utility Functions
-- ============================================================================

local function GetClassColor(class)
  local colors = {
    WARRIOR = { r = 0.78, g = 0.61, b = 0.43 },
    PALADIN = { r = 0.96, g = 0.55, b = 0.73 },
    HUNTER = { r = 0.67, g = 0.83, b = 0.45 },
    ROGUE = { r = 1.00, g = 0.96, b = 0.41 },
    PRIEST = { r = 1.00, g = 1.00, b = 1.00 },
    DEATHKNIGHT = { r = 0.77, g = 0.12, b = 0.23 },
    SHAMAN = { r = 0.00, g = 0.44, b = 0.87 },
    MAGE = { r = 0.25, g = 0.78, b = 0.92 },
    WARLOCK = { r = 0.53, g = 0.53, b = 0.93 },
    MONK = { r = 0.00, g = 1.00, b = 0.59 },
    DRUID = { r = 1.00, g = 0.49, b = 0.04 },
    DEMONHUNTER = { r = 0.64, g = 0.19, b = 0.79 },
  }

  return colors[class] or { r = 1, g = 1, b = 1 }
end

local function FormatHealthPercent(current, max)
  if max == 0 then
    return "0%"
  end

  local percent = (current / max) * 100
  return string.format("%.1f%%", percent)
end

local function IsPlayerInRange(unit)
  if not UnitExists(unit) then
    return false
  end

  return CheckInteractDistance(unit, 4)
end

local function GetUnitRole(unit)
  local role = UnitGroupRolesAssigned(unit)

  if role == "TANK" then
    return "Tank"
  elseif role == "HEALER" then
    return "Healer"
  elseif role == "DAMAGER" then
    return "DPS"
  else
    return "Unknown"
  end
end

-- ============================================================================
-- Event Callbacks and Hooks
-- ============================================================================

local function OnAddonLoaded(self, event, addonName)
  if addonName == "RaidHelper" then
    RaidHelper:Initialize()
  end
end

local function OnPlayerLogin(self)
  RaidHelper:OnEnable()
end

local function OnPlayerLogout(self)
  RaidHelper:OnDisable()
  RaidHelperDB:Save()
end

-- Register addon load events
local loaderFrame = CreateFrame("Frame")
loaderFrame:RegisterEvent("ADDON_LOADED")
loaderFrame:RegisterEvent("PLAYER_LOGIN")
loaderFrame:RegisterEvent("PLAYER_LOGOUT")
loaderFrame:SetScript("OnEvent", function(self, event, ...)
  if event == "ADDON_LOADED" then
    OnAddonLoaded(self, event, ...)
  elseif event == "PLAYER_LOGIN" then
    OnPlayerLogin(self)
  elseif event == "PLAYER_LOGOUT" then
    OnPlayerLogout(self)
  end
end)

-- ============================================================================
-- Combat Log Parsing
-- ============================================================================

local function ParseCombatLog()
  local timestamp, eventType, _, sourceGUID, sourceName, _, _, destGUID, destName = CombatLogGetCurrentEventInfo()

  if eventType == "SPELL_CAST_SUCCESS" then
    RaidHelper:PrintMessage(string.format("%s cast a spell", sourceName))
  elseif eventType == "SPELL_DAMAGE" then
    RaidHelper:PrintMessage(string.format("%s took damage", destName))
  elseif eventType == "SPELL_HEAL" then
    RaidHelper:PrintMessage(string.format("%s was healed", destName))
  end
end

-- Register combat log events
local combatFrame = CreateFrame("Frame")
combatFrame:RegisterEvent("COMBAT_LOG_EVENT_UNFILTERED")
combatFrame:SetScript("OnEvent", ParseCombatLog)

-- ============================================================================
-- Minimap Button (LibDBIcon integration)
-- ============================================================================

local MinimapButton = {
  icon = "Interface\\Icons\\Ability_Warrior_RallyingCry",
}

function MinimapButton:Initialize()
  local LDB = LibStub("LibDataBroker-1.1")
  local icon = LibStub("LibDBIcon-1.0")

  local dataObject = LDB:NewDataObject("RaidHelper", {
    type = "launcher",
    icon = self.icon,
    OnClick = function(_, button)
      if button == "LeftButton" then
        RaidHelper:ShowRaidFrame()
      elseif button == "RightButton" then
        LibStub("AceConfigDialog-3.0"):Open("RaidHelper")
      end
    end,
    OnTooltipShow = function(tooltip)
      tooltip:AddLine("Raid Helper")
      tooltip:AddLine("Left-click: Show raid frame")
      tooltip:AddLine("Right-click: Open config")
    end,
  })

  icon:Register("RaidHelper", dataObject, RaidHelperDB:Get("minimap"))
end

-- Initialize minimap button on load
MinimapButton:Initialize()

-- ============================================================================
-- Group Finder Integration
-- ============================================================================

local function CreateGroupFinderListing()
  local activityID = C_LFGList.GetActivityInfoTable(1)

  C_LFGList.CreateListing({
    activityID = activityID,
    name = "Raid Helper Group",
    comment = "Looking for more!",
    voiceChat = "",
    itemLevel = 0,
    honorLevel = 0,
  })

  RaidHelper:PrintMessage("Group listing created")
end

-- Hook into group finder
hooksecurefunc("LFGListInviteDialog_Show", function()
  RaidHelper:PrintMessage("Group finder opened")
end)

return RaidHelper
