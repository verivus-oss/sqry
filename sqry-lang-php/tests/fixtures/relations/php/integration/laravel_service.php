<?php
/**
 * Laravel Service Integration Fixture
 *
 * Represents a realistic Laravel service layer implementation with:
 * - Dependency injection
 * - Eloquent ORM patterns
 * - Events and listeners
 * - Validation
 * - Transactions
 * - Cache usage
 *
 * Ground truth annotations mark expected call and export edges.
 */

namespace App\Services;

use App\Models\User;
use App\Models\Role;
use App\Models\Permission;
use App\Events\UserCreated;
use App\Events\UserUpdated;
use App\Events\UserDeleted;
use App\Repositories\UserRepository;
use App\Mail\WelcomeEmail;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Cache;
use Illuminate\Support\Facades\Hash;
use Illuminate\Support\Facades\Mail;
use Illuminate\Support\Facades\Event;
use Illuminate\Support\Facades\Validator;
use Illuminate\Support\Str;

// EXPORT: UserService
class UserService
{
    private $repository;
    private $roleService;
    private $notificationService;

    // EXPORT: __construct
    public function __construct(
        UserRepository $repository,
        RoleService $roleService,
        NotificationService $notificationService
    ) {
        $this->repository = $repository;
        $this->roleService = $roleService;
        $this->notificationService = $notificationService;
    }

    // CALL: UserService::validateUserData
    // CALL: DB::beginTransaction
    // CALL: UserService::createUserRecord
    // CALL: UserService::assignDefaultRole
    // CALL: UserService::sendWelcomeNotification
    // CALL: Event::dispatch
    // CALL: DB::commit
    // CALL: DB::rollBack
    // EXPORT: createUser
    public function createUser(array $data)
    {
        $this->validateUserData($data);

        try {
            DB::beginTransaction();

            $user = $this->createUserRecord($data);
            $this->assignDefaultRole($user);
            $this->sendWelcomeNotification($user);

            Event::dispatch(new UserCreated($user));

            DB::commit();

            return $user;
        } catch (\Exception $e) {
            DB::rollBack();
            throw $e;
        }
    }

    // CALL: Cache::remember
    // CALL: UserRepository::findById
    // EXPORT: getUser
    public function getUser($id)
    {
        return Cache::remember("user.{$id}", 3600, function () use ($id) {
            return $this->repository->findById($id);
        });
    }

    // CALL: UserService::getUser
    // CALL: UserService::validateUpdateData
    // CALL: DB::beginTransaction
    // CALL: User::update
    // CALL: UserService::syncRoles
    // CALL: Cache::forget
    // CALL: Event::dispatch
    // CALL: DB::commit
    // CALL: DB::rollBack
    // EXPORT: updateUser
    public function updateUser($id, array $data)
    {
        $user = $this->getUser($id);

        if (!$user) {
            throw new \Exception('User not found');
        }

        $this->validateUpdateData($data);

        try {
            DB::beginTransaction();

            $user->update($data);

            if (isset($data['roles'])) {
                $this->syncRoles($user, $data['roles']);
            }

            Cache::forget("user.{$id}");

            Event::dispatch(new UserUpdated($user));

            DB::commit();

            return $user;
        } catch (\Exception $e) {
            DB::rollBack();
            throw $e;
        }
    }

    // CALL: UserService::getUser
    // CALL: DB::beginTransaction
    // CALL: User::delete
    // CALL: Cache::forget
    // CALL: Event::dispatch
    // CALL: DB::commit
    // CALL: DB::rollBack
    // EXPORT: deleteUser
    public function deleteUser($id)
    {
        $user = $this->getUser($id);

        if (!$user) {
            throw new \Exception('User not found');
        }

        try {
            DB::beginTransaction();

            $user->delete();

            Cache::forget("user.{$id}");

            Event::dispatch(new UserDeleted($user));

            DB::commit();

            return true;
        } catch (\Exception $e) {
            DB::rollBack();
            throw $e;
        }
    }

    // CALL: UserRepository::findByEmail
    // CALL: Hash::check
    // CALL: UserService::updateLastLogin
    // EXPORT: authenticateUser
    public function authenticateUser($email, $password)
    {
        $user = $this->repository->findByEmail($email);

        if (!$user) {
            return null;
        }

        if (!Hash::check($password, $user->password)) {
            return null;
        }

        $this->updateLastLogin($user);

        return $user;
    }

    // CALL: UserRepository::search
    // CALL: Cache::remember
    // EXPORT: searchUsers
    public function searchUsers(array $criteria)
    {
        $cacheKey = 'users.search.' . md5(json_encode($criteria));

        return Cache::remember($cacheKey, 600, function () use ($criteria) {
            return $this->repository->search($criteria);
        });
    }

    // CALL: User::roles
    // CALL: Collection::pluck
    // EXPORT: getUserRoles
    public function getUserRoles(User $user)
    {
        return $user->roles()->pluck('name');
    }

    // CALL: UserService::getUserRoles
    // CALL: RoleService::getPermissionsForRoles
    // EXPORT: getUserPermissions
    public function getUserPermissions(User $user)
    {
        $roles = $this->getUserRoles($user);
        return $this->roleService->getPermissionsForRoles($roles);
    }

    // CALL: UserService::getUserPermissions
    // CALL: Collection::contains
    // EXPORT: userHasPermission
    public function userHasPermission(User $user, $permission)
    {
        $permissions = $this->getUserPermissions($user);
        return $permissions->contains($permission);
    }

    // CALL: Validator::make
    // CALL: Validator::fails
    // CALL: Validator::errors
    // NOT EXPORTED: private
    private function validateUserData(array $data)
    {
        $validator = Validator::make($data, [
            'name' => 'required|string|max:255',
            'email' => 'required|email|unique:users',
            'password' => 'required|min:8|confirmed',
        ]);

        if ($validator->fails()) {
            throw new \Exception($validator->errors()->first());
        }
    }

    // CALL: Validator::make
    // CALL: Validator::fails
    // CALL: Validator::errors
    // NOT EXPORTED: private
    private function validateUpdateData(array $data)
    {
        $validator = Validator::make($data, [
            'name' => 'sometimes|string|max:255',
            'email' => 'sometimes|email',
            'password' => 'sometimes|min:8|confirmed',
        ]);

        if ($validator->fails()) {
            throw new \Exception($validator->errors()->first());
        }
    }

    // CALL: User::create
    // CALL: Hash::make
    // CALL: Str::random
    // NOT EXPORTED: private
    private function createUserRecord(array $data)
    {
        return User::create([
            'name' => $data['name'],
            'email' => $data['email'],
            'password' => Hash::make($data['password']),
            'email_verification_token' => Str::random(32),
            'created_at' => now(),
            'updated_at' => now(),
        ]);
    }

    // CALL: RoleService::getDefaultRole
    // CALL: User::roles
    // CALL: Relation::attach
    // NOT EXPORTED: private
    private function assignDefaultRole(User $user)
    {
        $defaultRole = $this->roleService->getDefaultRole();
        $user->roles()->attach($defaultRole->id);
    }

    // CALL: Mail::to
    // CALL: Mailer::send
    // NOT EXPORTED: private
    private function sendWelcomeNotification(User $user)
    {
        Mail::to($user->email)->send(new WelcomeEmail($user));
    }

    // CALL: User::roles
    // CALL: Relation::sync
    // NOT EXPORTED: private
    private function syncRoles(User $user, array $roleIds)
    {
        $user->roles()->sync($roleIds);
    }

    // CALL: User::update
    // NOT EXPORTED: private
    private function updateLastLogin(User $user)
    {
        $user->update([
            'last_login_at' => now(),
            'last_login_ip' => request()->ip(),
        ]);
    }

    // CALL: UserRepository::getUsersWithRoles
    // CALL: Cache::remember
    // EXPORT: getActiveUsersWithRoles
    public function getActiveUsersWithRoles()
    {
        return Cache::remember('users.active_with_roles', 1800, function () {
            return $this->repository->getUsersWithRoles(['status' => 'active']);
        });
    }

    // CALL: UserService::getUser
    // CALL: UserService::generatePasswordResetToken
    // CALL: NotificationService::sendPasswordResetEmail
    // EXPORT: initiatePasswordReset
    public function initiatePasswordReset($email)
    {
        $user = $this->repository->findByEmail($email);

        if (!$user) {
            return false;
        }

        $token = $this->generatePasswordResetToken($user);
        $this->notificationService->sendPasswordResetEmail($user, $token);

        return true;
    }

    // CALL: Str::random
    // CALL: Hash::make
    // CALL: DB::table
    // CALL: QueryBuilder::insert
    // NOT EXPORTED: private
    private function generatePasswordResetToken(User $user)
    {
        $token = Str::random(64);

        DB::table('password_resets')->insert([
            'email' => $user->email,
            'token' => Hash::make($token),
            'created_at' => now(),
        ]);

        return $token;
    }

    // CALL: DB::table
    // CALL: QueryBuilder::where
    // CALL: QueryBuilder::first
    // CALL: Hash::check
    // CALL: UserService::getUser
    // CALL: Hash::make
    // CALL: User::update
    // CALL: DB::table
    // CALL: QueryBuilder::where
    // CALL: QueryBuilder::delete
    // EXPORT: resetPassword
    public function resetPassword($email, $token, $newPassword)
    {
        $reset = DB::table('password_resets')
            ->where('email', $email)
            ->first();

        if (!$reset || !Hash::check($token, $reset->token)) {
            return false;
        }

        $user = $this->repository->findByEmail($email);
        $user->update([
            'password' => Hash::make($newPassword),
        ]);

        DB::table('password_resets')
            ->where('email', $email)
            ->delete();

        return true;
    }

    // CALL: UserRepository::count
    // CALL: Cache::remember
    // EXPORT: getTotalUsers
    public function getTotalUsers()
    {
        return Cache::remember('users.total_count', 3600, function () {
            return $this->repository->count();
        });
    }

    // CALL: UserRepository::getRecentlyActive
    // EXPORT: getRecentlyActiveUsers
    public function getRecentlyActiveUsers($limit = 10)
    {
        return $this->repository->getRecentlyActive($limit);
    }

    // CALL: User::roles
    // CALL: Collection::contains
    // EXPORT: userHasRole
    public function userHasRole(User $user, $roleName)
    {
        return $user->roles()->where('name', $roleName)->exists();
    }

    // CALL: UserService::userHasRole
    // EXPORT: userIsAdmin
    public function userIsAdmin(User $user)
    {
        return $this->userHasRole($user, 'admin');
    }

    // CALL: DB::table
    // CALL: QueryBuilder::where
    // CALL: QueryBuilder::whereBetween
    // CALL: QueryBuilder::count
    // EXPORT: getRegistrationStats
    public function getRegistrationStats($startDate, $endDate)
    {
        return DB::table('users')
            ->where('email_verified_at', '!=', null)
            ->whereBetween('created_at', [$startDate, $endDate])
            ->count();
    }

    // CALL: User::update
    // CALL: Cache::forget
    // EXPORT: verifyEmail
    public function verifyEmail($token)
    {
        $user = $this->repository->findByVerificationToken($token);

        if (!$user) {
            return false;
        }

        $user->update([
            'email_verified_at' => now(),
            'email_verification_token' => null,
        ]);

        Cache::forget("user.{$user->id}");

        return true;
    }

    // CALL: UserService::getUser
    // CALL: Str::random
    // CALL: User::update
    // CALL: NotificationService::sendVerificationEmail
    // EXPORT: resendVerificationEmail
    public function resendVerificationEmail($userId)
    {
        $user = $this->getUser($userId);

        if (!$user || $user->email_verified_at) {
            return false;
        }

        $token = Str::random(32);
        $user->update([
            'email_verification_token' => $token,
        ]);

        $this->notificationService->sendVerificationEmail($user, $token);

        return true;
    }

    // CALL: UserRepository::findByIds
    // CALL: Collection::map
    // EXPORT: bulkUpdateStatus
    public function bulkUpdateStatus(array $userIds, $status)
    {
        $users = $this->repository->findByIds($userIds);

        return $users->map(function ($user) use ($status) {
            $user->update(['status' => $status]);
            Cache::forget("user.{$user->id}");
            return $user;
        });
    }

    // CALL: UserRepository::exportUsers
    // CALL: Collection::map
    // EXPORT: exportToCsv
    public function exportToCsv(array $criteria = [])
    {
        $users = $this->repository->search($criteria);

        return $users->map(function ($user) {
            return [
                'id' => $user->id,
                'name' => $user->name,
                'email' => $user->email,
                'created_at' => $user->created_at->format('Y-m-d H:i:s'),
                'status' => $user->status,
            ];
        });
    }
}

// EXPORT: RoleService
class RoleService
{
    private $repository;

    // EXPORT: __construct
    public function __construct($repository)
    {
        $this->repository = $repository;
    }

    // CALL: Role::where
    // CALL: QueryBuilder::first
    // EXPORT: getDefaultRole
    public function getDefaultRole()
    {
        return Role::where('is_default', true)->first();
    }

    // CALL: Role::whereIn
    // CALL: QueryBuilder::with
    // CALL: QueryBuilder::get
    // CALL: Collection::pluck
    // CALL: Collection::flatten
    // CALL: Collection::unique
    // EXPORT: getPermissionsForRoles
    public function getPermissionsForRoles($roleNames)
    {
        $roles = Role::whereIn('name', $roleNames)
            ->with('permissions')
            ->get();

        return $roles->pluck('permissions')
            ->flatten()
            ->pluck('name')
            ->unique();
    }
}

// EXPORT: NotificationService
class NotificationService
{
    // CALL: Mail::to
    // CALL: Mailer::send
    // EXPORT: sendPasswordResetEmail
    public function sendPasswordResetEmail($user, $token)
    {
        Mail::to($user->email)->send(new \App\Mail\PasswordResetEmail($user, $token));
    }

    // CALL: Mail::to
    // CALL: Mailer::send
    // EXPORT: sendVerificationEmail
    public function sendVerificationEmail($user, $token)
    {
        Mail::to($user->email)->send(new \App\Mail\VerificationEmail($user, $token));
    }
}
