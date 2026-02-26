-- ============================================================================
-- Love2D Game: Space Shooter
-- A realistic Love2D game demonstrating common game patterns
-- ============================================================================
--
-- GROUND TRUTH ANNOTATIONS:
--
-- EXPORTS (28 total):
-- 1. love.load (function, line 91)
-- 2. love.update (function, line 134)
-- 3. love.draw (function, line 162)
-- 4. love.keypressed (function, line 197)
-- 5. love.keyreleased (function, line 217)
-- 6. love.mousepressed (function, line 234)
-- 7. Game (table, line 257)
-- 8. Game::new (method, colon, line 259)
-- 9. Game::reset (method, colon, line 273)
-- 10. Game::update (method, colon, line 290)
-- 11. Game::draw (method, colon, line 319)
-- 12. Game::gameOver (method, colon, line 350)
-- 13. Player (table, line 374)
-- 14. Player::new (method, colon, line 376)
-- 15. Player::update (method, colon, line 395)
-- 16. Player::draw (method, colon, line 425)
-- 17. Player::shoot (method, colon, line 447)
-- 18. Player::takeDamage (method, colon, line 466)
-- 19. Enemy (table, line 488)
-- 20. Enemy::new (method, colon, line 490)
-- 21. Enemy::update (method, colon, line 508)
-- 22. Enemy::draw (method, colon, line 530)
-- 23. Enemy::shoot (method, colon, line 549)
-- 24. Bullet (table, line 570)
-- 25. Bullet::new (method, colon, line 572)
-- 26. Bullet::update (method, colon, line 588)
-- 27. Bullet::draw (method, colon, line 604)
-- 28. Collision (table, line 619)
--
-- CALLS (91 total):
-- 1. love.window.setMode (line 98)
-- 2. love.window.setTitle (line 99)
-- 3. love.graphics.setDefaultFilter (line 102)
-- 4. love.graphics.setBackgroundColor (line 105)
-- 5. Game:new (line 110, colon)
-- 6. love.audio.newSource (line 115)
-- 7. love.audio.newSource (line 116)
-- 8. love.audio.newSource (line 117)
-- 9. background_music:setLooping (line 120, colon)
-- 10. background_music:play (line 121, colon)
-- 11. love.math.setRandomSeed (line 125)
-- 12. game:reset (line 128, colon)
-- 13. game:update (line 141, colon)
-- 14. self:spawnEnemies (line 145, colon)
-- 15. self:checkCollisions (line 148, colon)
-- 16. self:cleanupEntities (line 151, colon)
-- 17. love.audio.stop (line 155)
-- 18. love.event.quit (line 156)
-- ... (and 73 more calls throughout the file)
--
-- ============================================================================

-- Game module namespace
local Game
local Player
local Enemy
local Bullet
local Collision
local Particles
local Audio
local Input

-- Global game state
local game
local player
local enemies
local bullets
local particles
local score
local lives
local gameState
local dt_accumulator

-- Asset storage
local images = {}
local sounds = {}
local fonts = {}

-- Configuration
local Config = {
  window = {
    width = 800,
    height = 600,
    title = "Space Shooter",
  },
  player = {
    speed = 200,
    fireRate = 0.2,
  },
  enemy = {
    speed = 50,
    spawnRate = 2.0,
  },
  bullet = {
    speed = 400,
  },
}

-- ============================================================================
-- Love2D Callbacks
-- ============================================================================

function love.load()
  -- Set up window
  love.window.setMode(Config.window.width, Config.window.height)
  love.window.setTitle(Config.window.title)

  -- Set graphics settings
  love.graphics.setDefaultFilter("nearest", "nearest")

  -- Set background color
  love.graphics.setBackgroundColor(0.1, 0.1, 0.2)

  -- Initialize game
  game = Game:new()

  -- Load audio
  sounds.shoot = love.audio.newSource("assets/sounds/shoot.wav", "static")
  sounds.explosion = love.audio.newSource("assets/sounds/explosion.wav", "static")
  sounds.background = love.audio.newSource("assets/sounds/music.mp3", "stream")

  -- Start background music
  sounds.background:setLooping(true)
  sounds.background:play()

  -- Initialize random seed
  love.math.setRandomSeed(os.time())

  -- Reset game
  game:reset()
end

function love.update(dt)
  if gameState ~= "playing" then
    return
  end

  -- Update game
  game:update(dt)

  -- Spawn enemies
  game:spawnEnemies(dt)

  -- Check collisions
  game:checkCollisions()

  -- Cleanup dead entities
  game:cleanupEntities()

  -- Check for quit
  if love.keyboard.isDown("escape") then
    love.audio.stop()
    love.event.quit()
  end
end

function love.draw()
  -- Draw game
  game:draw()

  -- Draw UI
  love.graphics.setColor(1, 1, 1)
  love.graphics.print("Score: " .. score, 10, 10)
  love.graphics.print("Lives: " .. lives, 10, 30)

  -- Draw FPS
  love.graphics.print("FPS: " .. love.timer.getFPS(), Config.window.width - 80, 10)

  -- Game over screen
  if gameState == "gameover" then
    love.graphics.setColor(1, 0, 0)
    love.graphics.printf("GAME OVER", 0, Config.window.height / 2 - 20, Config.window.width, "center")
    love.graphics.printf("Press R to restart", 0, Config.window.height / 2 + 10, Config.window.width, "center")
  end

  -- Paused screen
  if gameState == "paused" then
    love.graphics.setColor(1, 1, 0)
    love.graphics.printf("PAUSED", 0, Config.window.height / 2, Config.window.width, "center")
  end
end

function love.keypressed(key)
  if key == "space" then
    if player then
      player:shoot()
    end
  end

  if key == "r" then
    if gameState == "gameover" then
      game:reset()
      gameState = "playing"
    end
  end

  if key == "p" then
    if gameState == "playing" then
      gameState = "paused"
    elseif gameState == "paused" then
      gameState = "playing"
    end
  end
end

function love.keyreleased(key)
  -- Handle key release events
  if key == "space" then
    if player then
      player.shooting = false
    end
  end
end

function love.mousepressed(x, y, button)
  if button == 1 then
    if player then
      player:shoot()
    end
  end
end

-- ============================================================================
-- Game Class
-- ============================================================================

Game = {}
Game.__index = Game

function Game:new()
  local instance = {
    state = "playing",
    spawnTimer = 0,
    enemySpawnRate = Config.enemy.spawnRate,
  }
  setmetatable(instance, Game)
  return instance
end

function Game:reset()
  -- Reset game state
  gameState = "playing"
  score = 0
  lives = 3

  -- Create player
  player = Player:new(Config.window.width / 2, Config.window.height - 50)

  -- Reset entities
  enemies = {}
  bullets = {}
  particles = {}

  self.spawnTimer = 0
end

function Game:update(dt)
  -- Update player
  if player then
    player:update(dt)
  end

  -- Update enemies
  for i, enemy in ipairs(enemies) do
    enemy:update(dt)

    -- Enemy shooting
    if love.math.random() < 0.01 then
      enemy:shoot()
    end
  end

  -- Update bullets
  for i, bullet in ipairs(bullets) do
    bullet:update(dt)
  end

  -- Update particles
  for i, particle in ipairs(particles) do
    particle:update(dt)
  end
end

function Game:draw()
  -- Draw player
  if player then
    player:draw()
  end

  -- Draw enemies
  for i, enemy in ipairs(enemies) do
    enemy:draw()
  end

  -- Draw bullets
  for i, bullet in ipairs(bullets) do
    bullet:draw()
  end

  -- Draw particles
  for i, particle in ipairs(particles) do
    love.graphics.setColor(particle.color)
    love.graphics.circle("fill", particle.x, particle.y, particle.size)
  end
end

function Game:gameOver()
  gameState = "gameover"

  -- Play game over sound
  if sounds.explosion then
    sounds.explosion:play()
  end

  -- Save high score
  local highScore = love.filesystem.read("highscore.txt")
  if not highScore or score > tonumber(highScore) then
    love.filesystem.write("highscore.txt", tostring(score))
  end
end

-- ============================================================================
-- Player Class
-- ============================================================================

Player = {}
Player.__index = Player

function Player:new(x, y)
  local instance = {
    x = x,
    y = y,
    width = 32,
    height = 32,
    speed = Config.player.speed,
    health = 100,
    shooting = false,
    fireTimer = 0,
    fireRate = Config.player.fireRate,
  }
  setmetatable(instance, Player)
  return instance
end

function Player:update(dt)
  -- Handle input
  if love.keyboard.isDown("left") or love.keyboard.isDown("a") then
    self.x = self.x - self.speed * dt
  end

  if love.keyboard.isDown("right") or love.keyboard.isDown("d") then
    self.x = self.x + self.speed * dt
  end

  if love.keyboard.isDown("up") or love.keyboard.isDown("w") then
    self.y = self.y - self.speed * dt
  end

  if love.keyboard.isDown("down") or love.keyboard.isDown("s") then
    self.y = self.y + self.speed * dt
  end

  -- Keep player in bounds
  self.x = math.max(0, math.min(self.x, Config.window.width - self.width))
  self.y = math.max(0, math.min(self.y, Config.window.height - self.height))

  -- Update fire timer
  self.fireTimer = self.fireTimer - dt
end

function Player:draw()
  -- Draw player ship
  love.graphics.setColor(0, 1, 0)
  love.graphics.rectangle("fill", self.x, self.y, self.width, self.height)

  -- Draw health bar
  love.graphics.setColor(1, 0, 0)
  love.graphics.rectangle("fill", self.x, self.y - 10, self.width, 5)
  love.graphics.setColor(0, 1, 0)
  love.graphics.rectangle("fill", self.x, self.y - 10, self.width * (self.health / 100), 5)
end

function Player:shoot()
  if self.fireTimer > 0 then
    return
  end

  -- Create bullet
  local bullet = Bullet:new(self.x + self.width / 2, self.y, 0, -1, "player")
  table.insert(bullets, bullet)

  -- Reset fire timer
  self.fireTimer = self.fireRate

  -- Play sound
  if sounds.shoot then
    sounds.shoot:play()
  end
end

function Player:takeDamage(amount)
  self.health = self.health - amount

  if self.health <= 0 then
    -- Player died
    lives = lives - 1

    if lives <= 0 then
      game:gameOver()
    else
      -- Respawn
      self.health = 100
      self.x = Config.window.width / 2
      self.y = Config.window.height - 50
    end
  end
end

-- ============================================================================
-- Enemy Class
-- ============================================================================

Enemy = {}
Enemy.__index = Enemy

function Enemy:new(x, y)
  local instance = {
    x = x,
    y = y,
    width = 32,
    height = 32,
    speed = Config.enemy.speed,
    health = 50,
    direction = 1,
  }
  setmetatable(instance, Enemy)
  return instance
end

function Enemy:update(dt)
  -- Move enemy
  self.y = self.y + self.speed * dt
  self.x = self.x + math.sin(self.y * 0.01) * self.speed * 0.5 * dt

  -- Bounce off walls
  if self.x < 0 or self.x > Config.window.width - self.width then
    self.direction = -self.direction
  end

  -- Remove if off screen
  if self.y > Config.window.height then
    self.dead = true
  end
end

function Enemy:draw()
  -- Draw enemy ship
  love.graphics.setColor(1, 0, 0)
  love.graphics.rectangle("fill", self.x, self.y, self.width, self.height)

  -- Draw health bar
  love.graphics.setColor(0, 1, 0)
  love.graphics.rectangle("fill", self.x, self.y - 10, self.width * (self.health / 50), 5)
end

function Enemy:shoot()
  -- Create bullet
  local bullet = Bullet:new(self.x + self.width / 2, self.y + self.height, 0, 1, "enemy")
  table.insert(bullets, bullet)

  -- Play sound
  if sounds.shoot then
    sounds.shoot:play()
  end
end

-- ============================================================================
-- Bullet Class
-- ============================================================================

Bullet = {}
Bullet.__index = Bullet

function Bullet:new(x, y, dx, dy, owner)
  local instance = {
    x = x,
    y = y,
    dx = dx,
    dy = dy,
    speed = Config.bullet.speed,
    owner = owner,
    radius = 5,
    dead = false,
  }
  setmetatable(instance, Bullet)
  return instance
end

function Bullet:update(dt)
  -- Move bullet
  self.x = self.x + self.dx * self.speed * dt
  self.y = self.y + self.dy * self.speed * dt

  -- Remove if off screen
  if self.y < 0 or self.y > Config.window.height then
    self.dead = true
  end
end

function Bullet:draw()
  -- Draw bullet
  if self.owner == "player" then
    love.graphics.setColor(1, 1, 0)
  else
    love.graphics.setColor(1, 0.5, 0)
  end

  love.graphics.circle("fill", self.x, self.y, self.radius)
end

-- ============================================================================
-- Collision Detection
-- ============================================================================

Collision = {}

function Collision.checkAABB(x1, y1, w1, h1, x2, y2, w2, h2)
  return x1 < x2 + w2 and
         x2 < x1 + w1 and
         y1 < y2 + h2 and
         y2 < y1 + h1
end

function Collision.checkCircle(x1, y1, r1, x2, y2, r2)
  local dx = x1 - x2
  local dy = y1 - y2
  local distance = math.sqrt(dx * dx + dy * dy)
  return distance < r1 + r2
end

function Game:checkCollisions()
  -- Check bullet-enemy collisions
  for i, bullet in ipairs(bullets) do
    if bullet.owner == "player" then
      for j, enemy in ipairs(enemies) do
        if Collision.checkCircle(bullet.x, bullet.y, bullet.radius, enemy.x + enemy.width / 2, enemy.y + enemy.height / 2, enemy.width / 2) then
          -- Bullet hit enemy
          bullet.dead = true
          enemy.health = enemy.health - 25

          if enemy.health <= 0 then
            enemy.dead = true
            score = score + 100

            -- Create explosion particles
            self:createExplosion(enemy.x + enemy.width / 2, enemy.y + enemy.height / 2)

            -- Play explosion sound
            if sounds.explosion then
              sounds.explosion:play()
            end
          end
        end
      end
    end
  end

  -- Check bullet-player collisions
  for i, bullet in ipairs(bullets) do
    if bullet.owner == "enemy" and player then
      if Collision.checkCircle(bullet.x, bullet.y, bullet.radius, player.x + player.width / 2, player.y + player.height / 2, player.width / 2) then
        -- Bullet hit player
        bullet.dead = true
        player:takeDamage(10)

        -- Create explosion particles
        self:createExplosion(player.x + player.width / 2, player.y + player.height / 2)
      end
    end
  end

  -- Check enemy-player collisions
  for i, enemy in ipairs(enemies) do
    if player and Collision.checkAABB(enemy.x, enemy.y, enemy.width, enemy.height, player.x, player.y, player.width, player.height) then
      -- Enemy hit player
      enemy.dead = true
      player:takeDamage(25)

      -- Create explosion
      self:createExplosion(enemy.x + enemy.width / 2, enemy.y + enemy.height / 2)
    end
  end
end

function Game:cleanupEntities()
  -- Remove dead bullets
  for i = #bullets, 1, -1 do
    if bullets[i].dead then
      table.remove(bullets, i)
    end
  end

  -- Remove dead enemies
  for i = #enemies, 1, -1 do
    if enemies[i].dead then
      table.remove(enemies, i)
    end
  end

  -- Remove dead particles
  for i = #particles, 1, -1 do
    if particles[i].dead then
      table.remove(particles, i)
    end
  end
end

function Game:spawnEnemies(dt)
  self.spawnTimer = self.spawnTimer + dt

  if self.spawnTimer >= self.enemySpawnRate then
    self.spawnTimer = 0

    -- Spawn enemy at random x position
    local x = love.math.random(0, Config.window.width - 32)
    local enemy = Enemy:new(x, -32)
    table.insert(enemies, enemy)
  end
end

function Game:createExplosion(x, y)
  for i = 1, 20 do
    local angle = love.math.random() * math.pi * 2
    local speed = love.math.random(50, 150)
    local particle = {
      x = x,
      y = y,
      vx = math.cos(angle) * speed,
      vy = math.sin(angle) * speed,
      size = love.math.random(2, 5),
      color = { love.math.random(), love.math.random(), love.math.random(), 1 },
      life = 1.0,
      dead = false,
      update = function(self, dt)
        self.x = self.x + self.vx * dt
        self.y = self.y + self.vy * dt
        self.life = self.life - dt
        if self.life <= 0 then
          self.dead = true
        end
      end,
    }
    table.insert(particles, particle)
  end
end

-- ============================================================================
-- Utility Functions
-- ============================================================================

local function clamp(value, min, max)
  return math.max(min, math.min(value, max))
end

local function lerp(a, b, t)
  return a + (b - a) * t
end

local function distance(x1, y1, x2, y2)
  local dx = x2 - x1
  local dy = y2 - y1
  return math.sqrt(dx * dx + dy * dy)
end

local function angle(x1, y1, x2, y2)
  return math.atan2(y2 - y1, x2 - x1)
end

-- ============================================================================
-- Debug Helpers
-- ============================================================================

local function drawDebugInfo()
  love.graphics.setColor(1, 1, 1)
  love.graphics.print("Enemies: " .. #enemies, 10, 50)
  love.graphics.print("Bullets: " .. #bullets, 10, 70)
  love.graphics.print("Particles: " .. #particles, 10, 90)
end

local function logGameState()
  print(string.format("Score: %d, Lives: %d, State: %s", score, lives, gameState))
end
