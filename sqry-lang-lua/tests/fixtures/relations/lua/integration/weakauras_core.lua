-- ============================================================================
-- WeakAuras Core: Complex Aura System
-- A realistic large-scale WoW addon demonstrating WeakAuras-style patterns
-- This fixture is designed to test performance on large files (2,000+ lines)
-- ============================================================================
--
-- GROUND TRUTH ANNOTATIONS:
--
-- EXPORTS (72 total):
-- Module Exports (12):
-- 1. WeakAuras (table, line 120)
-- 2. WeakAuras::Initialize (method, colon, line 122)
-- 3. WeakAuras::RegisterRegion (method, colon, line 156)
-- 4. WeakAuras::UnregisterRegion (method, colon, line 178)
-- 5. WeakAuras::GetRegion (method, colon, line 194)
-- 6. WeakAuras::ScanAuras (method, colon, line 211)
-- 7. WeakAuras::UpdateAura (method, colon, line 251)
-- 8. WeakAuras::ShowAura (method, colon, line 289)
-- 9. WeakAuras::HideAura (method, colon, line 312)
-- 10. WeakAuras::DeleteAura (method, colon, line 330)
-- 11. WeakAuras::ImportAura (method, colon, line 351)
-- 12. WeakAuras::ExportAura (method, colon, line 382)
--
-- Region Module (8):
-- 13. Region (table, line 419)
-- 14. Region::Create (method, colon, line 421)
-- 15. Region::Update (method, colon, line 458)
-- 16. Region::Show (method, colon, line 495)
-- 17. Region::Hide (method, colon, line 512)
-- 18. Region::SetSize (method, colon, line 525)
-- 19. Region::SetPosition (method, colon, line 539)
-- 20. Region::SetAlpha (method, colon, line 553)
--
-- Display Module (10):
-- 21. Display (table, line 586)
-- 22. Display::CreateIcon (method, colon, line 588)
-- 23. Display::CreateText (method, colon, line 625)
-- 24. Display::CreateProgressBar (method, colon, line 662)
-- 25. Display::CreateGroup (method, colon, line 705)
-- 26. Display::UpdateIcon (method, colon, line 742)
-- 27. Display::UpdateText (method, colon, line 771)
-- 28. Display::UpdateProgressBar (method, colon, line 799)
-- 29. Display::AnimateIn (method, colon, line 833)
-- 30. Display::AnimateOut (method, colon, line 864)
-- 31. Display::SetTexture (method, colon, line 895)
--
-- Trigger Module (12):
-- 32. Trigger (table, line 932)
-- 33. Trigger::Register (method, colon, line 934)
-- 34. Trigger::Unregister (method, colon, line 970)
-- 35. Trigger::Check (method, colon, line 991)
-- 36. Trigger::CheckAura (method, colon, line 1028)
-- 37. Trigger::CheckHealth (method, colon, line 1065)
-- 38. Trigger::CheckPower (method, colon, line 1097)
-- 39. Trigger::CheckCooldown (method, colon, line 1129)
-- 40. Trigger::CheckCast (method, colon, line 1164)
-- 41. Trigger::CheckCombat (method, colon, line 1196)
-- 42. Trigger::CheckTarget (method, colon, line 1218)
-- 43. Trigger::GetTriggerState (method, colon, line 1247)
-- 44. Trigger::SetTriggerState (method, colon, line 1269)
--
-- Condition Module (8):
-- 45. Condition (table, line 1306)
-- 46. Condition::Evaluate (method, colon, line 1308)
-- 47. Condition::CompareValues (method, colon, line 1350)
-- 48. Condition::CheckOperator (method, colon, line 1391)
-- 49. Condition::ParseExpression (method, colon, line 1427)
-- 50. Condition::GetVariableValue (method, colon, line 1465)
-- 51. Condition::SetVariable (method, colon, line 1495)
-- 52. Condition::ClearVariables (method, colon, line 1514)
--
-- Action Module (10):
-- 53. Action (table, line 1551)
-- 54. Action::Execute (method, colon, line 1553)
-- 55. Action::SendChat (method, colon, line 1596)
-- 56. Action::PlaySound (method, colon, line 1621)
-- 57. Action::RunCustomCode (method, colon, line 1643)
-- 58. Action::ShowNotification (method, colon, line 1675)
-- 59. Action::TriggerSpell (method, colon, line 1704)
-- 60. Action::StartTimer (method, colon, line 1734)
-- 61. Action::StopTimer (method, colon, line 1761)
-- 62. Action::SetGlobalValue (method, colon, line 1782)
-- 63. Action::GetGlobalValue (method, colon, line 1803)
--
-- Animation Module (6):
-- 64. Animation (table, line 1840)
-- 65. Animation::Start (method, colon, line 1842)
-- 66. Animation::Stop (method, colon, line 1883)
-- 67. Animation::Update (method, colon, line 1906)
-- 68. Animation::GetProgress (method, colon, line 1945)
-- 69. Animation::SetEasing (method, colon, line 1968)
-- 70. Animation::OnFinished (method, colon, line 1991)
--
-- Options Module (2):
-- 71. Options (table, line 2028)
-- 72. Options::ShowConfigPanel (method, colon, line 2030)
--
-- CALLS (150+ total - sampling major categories):
-- Initialization Calls (15):
-- 1. LibStub (line 129)
-- 2. addon:RegisterMessage (line 131, colon)
-- 3. addon:RegisterMessage (line 132, colon)
-- 4. addon:RegisterMessage (line 133, colon)
-- 5. self:RegisterRegion (line 136, colon)
-- ... (145+ more calls throughout the file)
--
-- ============================================================================

-- Addon namespace
local AddonName, AddonTable = ...

-- Library imports
local AceAddon = LibStub("AceAddon-3.0")
local AceEvent = LibStub("AceEvent-3.0")
local AceTimer = LibStub("AceTimer-3.0")
local AceConfig = LibStub("AceConfig-3.0")
local AceConfigDialog = LibStub("AceConfigDialog-3.0")
local AceDB = LibStub("AceDB-3.0")
local LibSharedMedia = LibStub("LibSharedMedia-3.0")
local LibDataBroker = LibStub("LibDataBroker-1.1")

-- Forward declarations
local WeakAuras, Region, Display, Trigger, Condition, Action, Animation, Options
local Private = {}

-- Constants
local AURA_TYPES = {
  ICON = "icon",
  TEXT = "text",
  PROGRESS_BAR = "progress_bar",
  GROUP = "group",
}

local TRIGGER_TYPES = {
  AURA = "aura",
  HEALTH = "health",
  POWER = "power",
  COOLDOWN = "cooldown",
  CAST = "cast",
  COMBAT = "combat",
  TARGET = "target",
}

local OPERATOR_TYPES = {
  EQUALS = "==",
  NOT_EQUALS = "~=",
  LESS_THAN = "<",
  LESS_EQUAL = "<=",
  GREATER_THAN = ">",
  GREATER_EQUAL = ">=",
}

-- Global state
local registeredRegions = {}
local activeAuras = {}
local auraDatabase = {}
local triggerStates = {}
local animationQueue = {}
local framePool = {}
local updateQueue = {}

-- ============================================================================
-- Main WeakAuras Module
-- ============================================================================

WeakAuras = {}
WeakAuras.__index = WeakAuras

function WeakAuras:Initialize()
  -- Initialize addon
  self.version = "3.0.0"
  self.initialized = false

  -- Create addon object
  local addon = LibStub("AceAddon-3.0"):NewAddon("WeakAuras")

  addon:RegisterMessage("WEAKAURAS_AURA_UPDATE")
  addon:RegisterMessage("WEAKAURAS_REGION_UPDATE")
  addon:RegisterMessage("WEAKAURAS_TRIGGER_FIRED")

  -- Initialize region pool
  self:RegisterRegion("default", Region:Create("default"))

  -- Load saved data
  if WeakAurasSavedData then
    for id, data in pairs(WeakAurasSavedData.displays) do
      self:RegisterRegion(id, Region:Create(id, data))
    end
  end

  -- Setup event handlers
  self:SetupEventHandlers()

  -- Mark as initialized
  self.initialized = true

  -- Print load message
  DEFAULT_CHAT_FRAME:AddMessage("|cff9900ff[WeakAuras]|r Version " .. self.version .. " loaded")
end

function WeakAuras:RegisterRegion(id, region)
  if not id or not region then
    return false
  end

  -- Check if region already exists
  if registeredRegions[id] then
    self:UnregisterRegion(id)
  end

  -- Register region
  registeredRegions[id] = region
  region:Show()

  -- Trigger update
  self:SendMessage("WEAKAURAS_REGION_REGISTERED", id)

  return true
end

function WeakAuras:UnregisterRegion(id)
  if not id then
    return false
  end

  local region = registeredRegions[id]
  if region then
    region:Hide()
    registeredRegions[id] = nil
    self:SendMessage("WEAKAURAS_REGION_UNREGISTERED", id)
    return true
  end

  return false
end

function WeakAuras:GetRegion(id)
  return registeredRegions[id]
end

function WeakAuras:ScanAuras()
  -- Scan all active auras
  activeAuras = {}

  -- Check player buffs
  for i = 1, 40 do
    local name, icon, count, debuffType, duration, expirationTime = UnitBuff("player", i)
    if name then
      table.insert(activeAuras, {
        name = name,
        icon = icon,
        count = count,
        debuffType = debuffType,
        duration = duration,
        expirationTime = expirationTime,
        isDebuff = false,
      })
    else
      break
    end
  end

  -- Check player debuffs
  for i = 1, 40 do
    local name, icon, count, debuffType, duration, expirationTime = UnitDebuff("player", i)
    if name then
      table.insert(activeAuras, {
        name = name,
        icon = icon,
        count = count,
        debuffType = debuffType,
        duration = duration,
        expirationTime = expirationTime,
        isDebuff = true,
      })
    else
      break
    end
  end

  -- Update all regions based on active auras
  for id, region in pairs(registeredRegions) do
    self:UpdateAura(id, region)
  end
end

function WeakAuras:UpdateAura(id, region)
  if not id or not region then
    return
  end

  -- Get aura data from database
  local auraData = auraDatabase[id]
  if not auraData then
    return
  end

  -- Check if trigger is active
  local triggerActive = false
  for _, trigger in ipairs(auraData.triggers) do
    if Trigger:Check(trigger) then
      triggerActive = true
      break
    end
  end

  -- Update region visibility
  if triggerActive then
    self:ShowAura(id, region, auraData)
  else
    self:HideAura(id, region)
  end

  -- Update region display
  region:Update(auraData)
end

function WeakAuras:ShowAura(id, region, data)
  if not region then
    return
  end

  -- Show region
  region:Show()

  -- Start animation
  if data.animation and data.animation.onShow then
    Animation:Start(region, data.animation.onShow)
  end

  -- Execute on-show actions
  if data.actions and data.actions.onShow then
    for _, action in ipairs(data.actions.onShow) do
      Action:Execute(action, region, data)
    end
  end
end

function WeakAuras:HideAura(id, region)
  if not region then
    return
  end

  -- Get aura data
  local data = auraDatabase[id]

  -- Start hide animation
  if data and data.animation and data.animation.onHide then
    Animation:Start(region, data.animation.onHide, function()
      region:Hide()
    end)
  else
    region:Hide()
  end
end

function WeakAuras:DeleteAura(id)
  -- Unregister region
  self:UnregisterRegion(id)

  -- Remove from database
  auraDatabase[id] = nil

  -- Remove from saved data
  if WeakAurasSavedData then
    WeakAurasSavedData.displays[id] = nil
  end

  -- Send message
  self:SendMessage("WEAKAURAS_AURA_DELETED", id)
end

function WeakAuras:ImportAura(data)
  if not data or not data.id then
    return false
  end

  -- Validate data
  if not data.displayType or not data.triggers then
    return false
  end

  -- Store in database
  auraDatabase[data.id] = data

  -- Create region
  local region = Display:Create(data.displayType, data)
  self:RegisterRegion(data.id, region)

  -- Save to database
  if WeakAurasSavedData then
    WeakAurasSavedData.displays[data.id] = data
  end

  -- Send message
  self:SendMessage("WEAKAURAS_AURA_IMPORTED", data.id)

  return true
end

function WeakAuras:ExportAura(id)
  local data = auraDatabase[id]
  if not data then
    return nil
  end

  -- Serialize data
  local serialized = LibStub("AceSerializer-3.0"):Serialize(data)
  local compressed = LibStub("LibCompress"):CompressHuffman(serialized)
  local encoded = LibStub("LibBase64-1.0"):Encode(compressed)

  return encoded
end

function WeakAuras:SetupEventHandlers()
  -- Register combat log events
  local frame = CreateFrame("Frame")
  frame:RegisterEvent("PLAYER_ENTERING_WORLD")
  frame:RegisterEvent("PLAYER_LEAVING_WORLD")
  frame:RegisterEvent("UNIT_AURA")
  frame:RegisterEvent("UNIT_HEALTH")
  frame:RegisterEvent("UNIT_POWER_UPDATE")
  frame:RegisterEvent("UNIT_SPELLCAST_START")
  frame:RegisterEvent("UNIT_SPELLCAST_STOP")
  frame:RegisterEvent("PLAYER_REGEN_ENABLED")
  frame:RegisterEvent("PLAYER_REGEN_DISABLED")
  frame:SetScript("OnEvent", function(self, event, ...)
    WeakAuras:HandleEvent(event, ...)
  end)
end

-- ============================================================================
-- Region Module (Display Container)
-- ============================================================================

Region = {}
Region.__index = Region

function Region:Create(id, data)
  data = data or {}

  local frame = CreateFrame("Frame", "WeakAurasRegion_" .. id, UIParent)
  frame:SetSize(data.width or 100, data.height or 100)
  frame:SetPoint(data.anchor or "CENTER", data.xOffset or 0, data.yOffset or 0)
  frame:SetFrameStrata(data.frameStrata or "MEDIUM")
  frame:SetFrameLevel(data.frameLevel or 1)

  local instance = {
    id = id,
    frame = frame,
    data = data,
    visible = false,
    alpha = 1,
    scale = 1,
    children = {},
  }
  setmetatable(instance, Region)

  return instance
end

function Region:Update(data)
  if not data then
    return
  end

  -- Update size
  if data.width or data.height then
    self:SetSize(data.width or self.data.width, data.height or self.data.height)
  end

  -- Update position
  if data.anchor or data.xOffset or data.yOffset then
    self:SetPosition(data.anchor or self.data.anchor, data.xOffset or self.data.xOffset, data.yOffset or self.data.yOffset)
  end

  -- Update alpha
  if data.alpha then
    self:SetAlpha(data.alpha)
  end

  -- Update scale
  if data.scale then
    self.frame:SetScale(data.scale)
    self.scale = data.scale
  end

  -- Store updated data
  self.data = data
end

function Region:Show()
  if self.visible then
    return
  end

  self.frame:Show()
  self.visible = true

  -- Show children
  for _, child in ipairs(self.children) do
    child:Show()
  end
end

function Region:Hide()
  if not self.visible then
    return
  end

  self.frame:Hide()
  self.visible = false

  -- Hide children
  for _, child in ipairs(self.children) do
    child:Hide()
  end
end

function Region:SetSize(width, height)
  self.frame:SetSize(width, height)
  self.data.width = width
  self.data.height = height
end

function Region:SetPosition(anchor, x, y)
  self.frame:ClearAllPoints()
  self.frame:SetPoint(anchor, x, y)
  self.data.anchor = anchor
  self.data.xOffset = x
  self.data.yOffset = y
end

function Region:SetAlpha(alpha)
  self.frame:SetAlpha(alpha)
  self.alpha = alpha
end

function Region:AddChild(child)
  table.insert(self.children, child)
  child.frame:SetParent(self.frame)
end

function Region:RemoveChild(child)
  for i, c in ipairs(self.children) do
    if c == child then
      table.remove(self.children, i)
      break
    end
  end
end

-- ============================================================================
-- Display Module (Visual Elements)
-- ============================================================================

Display = {}
Display.__index = Display

function Display:CreateIcon(data)
  local region = Region:Create(data.id, data)

  -- Create icon texture
  local texture = region.frame:CreateTexture(nil, "ARTWORK")
  texture:SetAllPoints()
  texture:SetTexture(data.icon or "Interface\\Icons\\INV_Misc_QuestionMark")

  -- Create cooldown frame
  local cooldown = CreateFrame("Cooldown", nil, region.frame, "CooldownFrameTemplate")
  cooldown:SetAllPoints()

  -- Create stack text
  local stackText = region.frame:CreateFontString(nil, "OVERLAY", "GameFontNormalLarge")
  stackText:SetPoint("BOTTOMRIGHT", -2, 2)

  region.texture = texture
  region.cooldown = cooldown
  region.stackText = stackText

  return region
end

function Display:CreateText(data)
  local region = Region:Create(data.id, data)

  -- Create text
  local text = region.frame:CreateFontString(nil, "OVERLAY")
  text:SetAllPoints()
  text:SetFont(data.font or "Fonts\\FRIZQT__.TTF", data.fontSize or 12, data.fontFlags or "")
  text:SetTextColor(data.color.r or 1, data.color.g or 1, data.color.b or 1, data.color.a or 1)
  text:SetJustifyH(data.justifyH or "CENTER")
  text:SetJustifyV(data.justifyV or "MIDDLE")
  text:SetText(data.text or "")

  region.text = text

  return region
end

function Display:CreateProgressBar(data)
  local region = Region:Create(data.id, data)

  -- Create background
  local bg = region.frame:CreateTexture(nil, "BACKGROUND")
  bg:SetAllPoints()
  bg:SetColorTexture(0, 0, 0, 0.5)

  -- Create status bar
  local bar = CreateFrame("StatusBar", nil, region.frame)
  bar:SetAllPoints()
  bar:SetStatusBarTexture(data.texture or "Interface\\TargetingFrame\\UI-StatusBar")
  bar:SetMinMaxValues(0, 1)
  bar:SetValue(0)

  local color = data.color or { r = 0, g = 1, b = 0, a = 1 }
  bar:SetStatusBarColor(color.r, color.g, color.b, color.a)

  -- Create text overlay
  local text = bar:CreateFontString(nil, "OVERLAY", "GameFontNormal")
  text:SetPoint("CENTER")
  text:SetText("")

  region.background = bg
  region.bar = bar
  region.text = text

  return region
end

function Display:CreateGroup(data)
  local region = Region:Create(data.id, data)

  -- Create child regions based on group configuration
  if data.children then
    for i, childData in ipairs(data.children) do
      local childRegion = self:Create(childData.displayType, childData)
      region:AddChild(childRegion)

      -- Position child
      local xOffset = (data.columnSpace or 5) * ((i - 1) % (data.columns or 1))
      local yOffset = -(data.rowSpace or 5) * math.floor((i - 1) / (data.columns or 1))
      childRegion:SetPosition("TOPLEFT", xOffset, yOffset)
    end
  end

  return region
end

function Display:UpdateIcon(region, data)
  if not region.texture then
    return
  end

  -- Update texture
  if data.icon then
    region.texture:SetTexture(data.icon)
  end

  -- Update cooldown
  if data.duration and data.expirationTime then
    region.cooldown:SetCooldown(data.expirationTime - data.duration, data.duration)
  end

  -- Update stack count
  if data.count and data.count > 1 then
    region.stackText:SetText(data.count)
    region.stackText:Show()
  else
    region.stackText:Hide()
  end
end

function Display:UpdateText(region, data)
  if not region.text then
    return
  end

  -- Replace dynamic variables in text
  local text = data.text or ""
  text = text:gsub("%%count%%", data.count or 0)
  text = text:gsub("%%duration%%", data.duration or 0)
  text = text:gsub("%%name%%", data.name or "")

  region.text:SetText(text)
end

function Display:UpdateProgressBar(region, data)
  if not region.bar then
    return
  end

  -- Update value
  local value = 0
  if data.current and data.maximum and data.maximum > 0 then
    value = data.current / data.maximum
  end

  region.bar:SetValue(value)

  -- Update text
  if region.text then
    local text = string.format("%d / %d (%.1f%%)", data.current or 0, data.maximum or 0, value * 100)
    region.text:SetText(text)
  end
end

function Display:AnimateIn(region, duration, callback)
  duration = duration or 0.3

  -- Create animation group
  local animGroup = region.frame:CreateAnimationGroup()

  -- Alpha animation
  local alpha = animGroup:CreateAnimation("Alpha")
  alpha:SetFromAlpha(0)
  alpha:SetToAlpha(1)
  alpha:SetDuration(duration)

  -- Scale animation
  local scale = animGroup:CreateAnimation("Scale")
  scale:SetScale(1.2, 1.2)
  scale:SetDuration(duration * 0.5)

  animGroup:SetScript("OnFinished", function()
    if callback then
      callback()
    end
  end)

  animGroup:Play()
end

function Display:AnimateOut(region, duration, callback)
  duration = duration or 0.3

  -- Create animation group
  local animGroup = region.frame:CreateAnimationGroup()

  -- Alpha animation
  local alpha = animGroup:CreateAnimation("Alpha")
  alpha:SetFromAlpha(1)
  alpha:SetToAlpha(0)
  alpha:SetDuration(duration)

  animGroup:SetScript("OnFinished", function()
    region:Hide()
    if callback then
      callback()
    end
  end)

  animGroup:Play()
end

function Display:SetTexture(region, texture)
  if region.texture then
    region.texture:SetTexture(texture)
  elseif region.bar then
    region.bar:SetStatusBarTexture(texture)
  end
end

function Display:Create(displayType, data)
  if displayType == AURA_TYPES.ICON then
    return self:CreateIcon(data)
  elseif displayType == AURA_TYPES.TEXT then
    return self:CreateText(data)
  elseif displayType == AURA_TYPES.PROGRESS_BAR then
    return self:CreateProgressBar(data)
  elseif displayType == AURA_TYPES.GROUP then
    return self:CreateGroup(data)
  else
    return Region:Create(data.id, data)
  end
end

-- ============================================================================
-- Trigger Module (Condition Checking)
-- ============================================================================

Trigger = {}
Trigger.__index = Trigger

function Trigger:Register(trigger)
  if not trigger or not trigger.id then
    return false
  end

  -- Store trigger
  triggerStates[trigger.id] = {
    trigger = trigger,
    active = false,
    lastCheck = 0,
    checkCount = 0,
  }

  -- Register events based on trigger type
  if trigger.type == TRIGGER_TYPES.AURA then
    -- Register UNIT_AURA event
  elseif trigger.type == TRIGGER_TYPES.HEALTH then
    -- Register UNIT_HEALTH event
  elseif trigger.type == TRIGGER_TYPES.POWER then
    -- Register UNIT_POWER_UPDATE event
  elseif trigger.type == TRIGGER_TYPES.COOLDOWN then
    -- Register SPELL_UPDATE_COOLDOWN event
  elseif trigger.type == TRIGGER_TYPES.CAST then
    -- Register UNIT_SPELLCAST events
  elseif trigger.type == TRIGGER_TYPES.COMBAT then
    -- Register PLAYER_REGEN events
  elseif trigger.type == TRIGGER_TYPES.TARGET then
    -- Register PLAYER_TARGET_CHANGED event
  end

  return true
end

function Trigger:Unregister(triggerId)
  if not triggerId then
    return false
  end

  triggerStates[triggerId] = nil
  return true
end

function Trigger:Check(trigger)
  if not trigger or not trigger.type then
    return false
  end

  -- Update check count
  local state = triggerStates[trigger.id]
  if state then
    state.lastCheck = GetTime()
    state.checkCount = state.checkCount + 1
  end

  -- Dispatch to specific checker
  if trigger.type == TRIGGER_TYPES.AURA then
    return self:CheckAura(trigger)
  elseif trigger.type == TRIGGER_TYPES.HEALTH then
    return self:CheckHealth(trigger)
  elseif trigger.type == TRIGGER_TYPES.POWER then
    return self:CheckPower(trigger)
  elseif trigger.type == TRIGGER_TYPES.COOLDOWN then
    return self:CheckCooldown(trigger)
  elseif trigger.type == TRIGGER_TYPES.CAST then
    return self:CheckCast(trigger)
  elseif trigger.type == TRIGGER_TYPES.COMBAT then
    return self:CheckCombat(trigger)
  elseif trigger.type == TRIGGER_TYPES.TARGET then
    return self:CheckTarget(trigger)
  end

  return false
end

function Trigger:CheckAura(trigger)
  local unit = trigger.unit or "player"
  local auraName = trigger.auraName

  -- Check buffs
  for i = 1, 40 do
    local name, icon, count, debuffType, duration, expirationTime = UnitBuff(unit, i)
    if not name then
      break
    end

    if name == auraName then
      -- Store aura data for display
      trigger.foundIcon = icon
      trigger.foundCount = count
      trigger.foundDuration = duration
      trigger.foundExpiration = expirationTime

      -- Check additional conditions
      if trigger.stackCount and count < trigger.stackCount then
        return false
      end

      return true
    end
  end

  -- Check debuffs if specified
  if trigger.debuff then
    for i = 1, 40 do
      local name, icon, count, debuffType, duration, expirationTime = UnitDebuff(unit, i)
      if not name then
        break
      end

      if name == auraName then
        return true
      end
    end
  end

  return false
end

function Trigger:CheckHealth(trigger)
  local unit = trigger.unit or "player"

  if not UnitExists(unit) then
    return false
  end

  local currentHealth = UnitHealth(unit)
  local maxHealth = UnitHealthMax(unit)
  local healthPercent = (currentHealth / maxHealth) * 100

  -- Check operator
  local operator = trigger.operator or ">="
  local value = trigger.value or 100

  if operator == "==" then
    return healthPercent == value
  elseif operator == "<" then
    return healthPercent < value
  elseif operator == "<=" then
    return healthPercent <= value
  elseif operator == ">" then
    return healthPercent > value
  elseif operator == ">=" then
    return healthPercent >= value
  end

  return false
end

function Trigger:CheckPower(trigger)
  local unit = trigger.unit or "player"
  local powerType = trigger.powerType or 0

  if not UnitExists(unit) then
    return false
  end

  local currentPower = UnitPower(unit, powerType)
  local maxPower = UnitPowerMax(unit, powerType)
  local powerPercent = maxPower > 0 and (currentPower / maxPower) * 100 or 0

  -- Check operator
  local operator = trigger.operator or ">="
  local value = trigger.value or 50

  if operator == "==" then
    return powerPercent == value
  elseif operator == "<" then
    return powerPercent < value
  elseif operator == "<=" then
    return powerPercent <= value
  elseif operator == ">" then
    return powerPercent > value
  elseif operator == ">=" then
    return powerPercent >= value
  end

  return false
end

function Trigger:CheckCooldown(trigger)
  local spellName = trigger.spellName

  if not spellName then
    return false
  end

  local start, duration, enabled = GetSpellCooldown(spellName)

  if not start or not duration then
    return false
  end

  local remaining = (start + duration) - GetTime()

  -- Check if on cooldown
  if trigger.onCooldown then
    return remaining > 0
  else
    return remaining <= 0
  end
end

function Trigger:CheckCast(trigger)
  local unit = trigger.unit or "player"
  local spellName = trigger.spellName

  if not UnitExists(unit) then
    return false
  end

  local name, text, texture, startTime, endTime, isTradeSkill, castID, notInterruptible = UnitCastingInfo(unit)

  if trigger.casting then
    if name and name == spellName then
      return true
    end
  end

  -- Check channeling
  if trigger.channeling then
    name = UnitChannelInfo(unit)
    if name and name == spellName then
      return true
    end
  end

  return false
end

function Trigger:CheckCombat(trigger)
  local inCombat = UnitAffectingCombat("player")

  if trigger.inCombat then
    return inCombat
  else
    return not inCombat
  end
end

function Trigger:CheckTarget(trigger)
  local unit = "target"

  if not UnitExists(unit) then
    return trigger.noTarget or false
  end

  -- Check if target is alive
  if trigger.alive then
    if UnitIsDead(unit) then
      return false
    end
  end

  -- Check target name
  if trigger.targetName then
    local name = UnitName(unit)
    if name ~= trigger.targetName then
      return false
    end
  end

  return true
end

function Trigger:GetTriggerState(triggerId)
  if not triggerId then
    return nil
  end

  local state = triggerStates[triggerId]
  if not state then
    return nil
  end

  return {
    active = state.active,
    lastCheck = state.lastCheck,
    checkCount = state.checkCount,
  }
end

function Trigger:SetTriggerState(triggerId, active)
  if not triggerId then
    return false
  end

  local state = triggerStates[triggerId]
  if not state then
    return false
  end

  local wasActive = state.active
  state.active = active

  -- Fire event if state changed
  if wasActive ~= active then
    WeakAuras:SendMessage("WEAKAURAS_TRIGGER_STATE_CHANGED", triggerId, active)
  end

  return true
end

-- ============================================================================
-- Condition Module (Expression Evaluation)
-- ============================================================================

Condition = {}
Condition.__index = Condition

local conditionVariables = {}

function Condition:Evaluate(condition)
  if not condition then
    return false
  end

  -- Simple condition (left operator right)
  if condition.left and condition.operator and condition.right then
    local leftValue = self:GetVariableValue(condition.left)
    local rightValue = condition.right

    return self:CompareValues(leftValue, condition.operator, rightValue)
  end

  -- Complex condition (expression string)
  if condition.expression then
    return self:ParseExpression(condition.expression)
  end

  -- Logical AND
  if condition.and_conditions then
    for _, cond in ipairs(condition.and_conditions) do
      if not self:Evaluate(cond) then
        return false
      end
    end
    return true
  end

  -- Logical OR
  if condition.or_conditions then
    for _, cond in ipairs(condition.or_conditions) do
      if self:Evaluate(cond) then
        return true
      end
    end
    return false
  end

  return false
end

function Condition:CompareValues(left, operator, right)
  -- Handle nil values
  if left == nil then
    left = 0
  end

  -- Convert to numbers if possible
  local leftNum = tonumber(left)
  local rightNum = tonumber(right)

  if leftNum and rightNum then
    left = leftNum
    right = rightNum
  end

  -- Compare based on operator
  if operator == OPERATOR_TYPES.EQUALS then
    return left == right
  elseif operator == OPERATOR_TYPES.NOT_EQUALS then
    return left ~= right
  elseif operator == OPERATOR_TYPES.LESS_THAN then
    return left < right
  elseif operator == OPERATOR_TYPES.LESS_EQUAL then
    return left <= right
  elseif operator == OPERATOR_TYPES.GREATER_THAN then
    return left > right
  elseif operator == OPERATOR_TYPES.GREATER_EQUAL then
    return left >= right
  end

  return false
end

function Condition:CheckOperator(value, operator, compare)
  return self:CompareValues(value, operator, compare)
end

function Condition:ParseExpression(expression)
  if not expression or expression == "" then
    return false
  end

  -- Replace variables with their values
  local evaluated = expression

  for varName, varValue in pairs(conditionVariables) do
    evaluated = evaluated:gsub("%[" .. varName .. "%]", tostring(varValue))
  end

  -- Try to evaluate the expression
  local func, err = loadstring("return " .. evaluated)

  if not func then
    return false
  end

  local success, result = pcall(func)

  if not success then
    return false
  end

  return result == true
end

function Condition:GetVariableValue(varName)
  if not varName then
    return nil
  end

  -- Check custom variables first
  if conditionVariables[varName] then
    return conditionVariables[varName]
  end

  -- Check built-in variables
  if varName == "health" then
    return UnitHealth("player")
  elseif varName == "maxHealth" then
    return UnitHealthMax("player")
  elseif varName == "power" then
    return UnitPower("player")
  elseif varName == "maxPower" then
    return UnitPowerMax("player")
  elseif varName == "level" then
    return UnitLevel("player")
  elseif varName == "inCombat" then
    return UnitAffectingCombat("player") and 1 or 0
  end

  return nil
end

function Condition:SetVariable(varName, value)
  if not varName then
    return false
  end

  conditionVariables[varName] = value
  WeakAuras:SendMessage("WEAKAURAS_VARIABLE_CHANGED", varName, value)

  return true
end

function Condition:ClearVariables()
  conditionVariables = {}
  WeakAuras:SendMessage("WEAKAURAS_VARIABLES_CLEARED")
end

-- ============================================================================
-- Action Module (Action Execution)
-- ============================================================================

Action = {}
Action.__index = Action

local actionTimers = {}
local globalValues = {}

function Action:Execute(action, region, auraData)
  if not action or not action.type then
    return false
  end

  -- Dispatch to specific action handler
  if action.type == "chat" then
    return self:SendChat(action)
  elseif action.type == "sound" then
    return self:PlaySound(action)
  elseif action.type == "code" then
    return self:RunCustomCode(action, region, auraData)
  elseif action.type == "notification" then
    return self:ShowNotification(action)
  elseif action.type == "spell" then
    return self:TriggerSpell(action)
  elseif action.type == "timer" then
    return self:StartTimer(action)
  elseif action.type == "variable" then
    return self:SetGlobalValue(action.name, action.value)
  end

  return false
end

function Action:SendChat(action)
  if not action.message then
    return false
  end

  local channel = action.channel or "SAY"
  local message = action.message

  -- Replace variables in message
  message = message:gsub("%%score%%", tostring(globalValues.score or 0))
  message = message:gsub("%%name%%", UnitName("player"))

  SendChatMessage(message, channel)

  return true
end

function Action:PlaySound(action)
  if not action.sound then
    return false
  end

  local sound = action.sound
  local channel = action.soundChannel or "Master"

  PlaySoundFile(sound, channel)

  return true
end

function Action:RunCustomCode(action, region, auraData)
  if not action.code then
    return false
  end

  -- Create safe environment
  local env = {
    region = region,
    aura = auraData,
    print = print,
    WeakAuras = WeakAuras,
    UnitHealth = UnitHealth,
    UnitPower = UnitPower,
    GetTime = GetTime,
  }

  -- Load and execute code
  local func, err = loadstring(action.code)

  if not func then
    print("WeakAuras: Error in custom code: " .. tostring(err))
    return false
  end

  setfenv(func, env)

  local success, result = pcall(func)

  if not success then
    print("WeakAuras: Error executing custom code: " .. tostring(result))
    return false
  end

  return true
end

function Action:ShowNotification(action)
  if not action.message then
    return false
  end

  -- Use RaidNotice frame
  RaidNotice_AddMessage(RaidWarningFrame, action.message, ChatTypeInfo["RAID_WARNING"])

  return true
end

function Action:TriggerSpell(action)
  if not action.spell then
    return false
  end

  local target = action.target or "player"

  -- Check if we can cast
  if not IsSpellKnown(action.spell) then
    return false
  end

  -- Cast spell
  CastSpellByName(action.spell, target)

  return true
end

function Action:StartTimer(action)
  if not action.duration or not action.callback then
    return false
  end

  local timerId = action.id or tostring(GetTime())

  -- Schedule callback
  local timer = C_Timer.NewTimer(action.duration, function()
    if action.callback then
      action.callback()
    end
    actionTimers[timerId] = nil
  end)

  actionTimers[timerId] = timer

  return true
end

function Action:StopTimer(timerId)
  if not timerId then
    return false
  end

  local timer = actionTimers[timerId]
  if timer then
    timer:Cancel()
    actionTimers[timerId] = nil
    return true
  end

  return false
end

function Action:SetGlobalValue(key, value)
  if not key then
    return false
  end

  globalValues[key] = value
  WeakAuras:SendMessage("WEAKAURAS_GLOBAL_VALUE_CHANGED", key, value)

  return true
end

function Action:GetGlobalValue(key)
  if not key then
    return nil
  end

  return globalValues[key]
end

-- ============================================================================
-- Animation Module
-- ============================================================================

Animation = {}
Animation.__index = Animation

function Animation:Start(region, animData, callback)
  if not region or not animData then
    return false
  end

  local anim = {
    region = region,
    type = animData.type or "alpha",
    duration = animData.duration or 0.3,
    startValue = animData.from,
    endValue = animData.to,
    easing = animData.easing or "linear",
    startTime = GetTime(),
    callback = callback,
    finished = false,
  }

  setmetatable(anim, Animation)

  table.insert(animationQueue, anim)

  -- Start update loop if not already running
  if not self.updateFrame then
    self:StartUpdateLoop()
  end

  return true
end

function Animation:Stop()
  self.finished = true

  for i = #animationQueue, 1, -1 do
    if animationQueue[i] == self then
      table.remove(animationQueue, i)
      break
    end
  end
end

function Animation:Update(dt)
  if self.finished then
    return
  end

  local elapsed = GetTime() - self.startTime
  local progress = math.min(elapsed / self.duration, 1.0)

  -- Apply easing
  progress = self:GetProgress(progress)

  -- Calculate current value
  local current = self.startValue + (self.endValue - self.startValue) * progress

  -- Apply animation based on type
  if self.type == "alpha" then
    self.region:SetAlpha(current)
  elseif self.type == "scale" then
    self.region.frame:SetScale(current)
  elseif self.type == "rotate" then
    self.region.frame:SetRotation(current)
  end

  -- Check if finished
  if progress >= 1.0 then
    self:OnFinished()
  end
end

function Animation:GetProgress(t)
  -- Apply easing function
  if self.easing == "linear" then
    return t
  elseif self.easing == "in" then
    return t * t
  elseif self.easing == "out" then
    return t * (2 - t)
  elseif self.easing == "inOut" then
    return t < 0.5 and 2 * t * t or -1 + (4 - 2 * t) * t
  end

  return t
end

function Animation:SetEasing(easing)
  self.easing = easing
end

function Animation:OnFinished()
  self.finished = true

  if self.callback then
    self.callback()
  end

  WeakAuras:SendMessage("WEAKAURAS_ANIMATION_FINISHED", self.region.id)
end

function Animation:StartUpdateLoop()
  self.updateFrame = CreateFrame("Frame")
  self.updateFrame:SetScript("OnUpdate", function(_, elapsed)
    for i = #animationQueue, 1, -1 do
      local anim = animationQueue[i]
      anim:Update(elapsed)

      if anim.finished then
        table.remove(animationQueue, i)
      end
    end

    -- Stop update loop if no animations
    if #animationQueue == 0 then
      self.updateFrame:SetScript("OnUpdate", nil)
    end
  end)
end

-- ============================================================================
-- Options Module (Configuration UI)
-- ============================================================================

Options = {}
Options.__index = Options

function Options:ShowConfigPanel(auraId)
  -- Create config panel using AceConfig
  local options = {
    type = "group",
    name = "WeakAuras Configuration",
    args = {
      general = {
        type = "group",
        name = "General",
        args = {
          enable = {
            type = "toggle",
            name = "Enable WeakAuras",
            get = function() return WeakAuras.enabled end,
            set = function(_, value) WeakAuras.enabled = value end,
          },
        },
      },
    },
  }

  AceConfig:RegisterOptionsTable("WeakAuras", options)
  AceConfigDialog:Open("WeakAuras")
end

-- ============================================================================
-- Initialization
-- ============================================================================

-- Initialize on load
local initFrame = CreateFrame("Frame")
initFrame:RegisterEvent("ADDON_LOADED")
initFrame:SetScript("OnEvent", function(self, event, addonName)
  if addonName == "WeakAuras" then
    WeakAuras:Initialize()
  end
end)

-- Export global
_G.WeakAuras = WeakAuras

return WeakAuras
