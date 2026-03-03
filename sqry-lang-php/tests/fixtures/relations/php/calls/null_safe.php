<?php
/**
 * Null-Safe Operator Call Extraction Fixture
 *
 * Tests null-safe operator ?-> for method calls and property access (PHP 8.0+).
 * Ground truth annotations mark expected call edges.
 */

namespace App\Models;

class User {
    private $profile;

    public function getProfile() {
        return $this->profile;
    }

    public function setProfile($profile) {
        $this->profile = $profile;
    }
}

class Profile {
    private $address;
    private $name;

    public function getName() {
        return $this->name;
    }

    public function getAddress() {
        return $this->address;
    }

    public function setAddress($address) {
        $this->address = $address;
    }
}

class Address {
    private $city;
    private $country;

    public function getCity() {
        return $this->city;
    }

    public function getCountry() {
        return $this->country;
    }

    // CALL: strtoupper
    public function getFormattedCity() {
        return strtoupper($this->city);
    }
}

namespace App\Services;

use App\Models\User;

class UserService {
    // CALL: User::getProfile
    // CALL: Profile::getName
    public function getUserName(?User $user) {
        // Null-safe chaining
        return $user?->getProfile()?->getName();
    }

    // CALL: User::getProfile
    // CALL: Profile::getAddress
    // CALL: Address::getCity
    public function getUserCity(?User $user) {
        // Null-safe chaining with multiple levels
        return $user?->getProfile()?->getAddress()?->getCity();
    }

    // CALL: User::getProfile
    // CALL: Profile::getAddress
    // CALL: Address::getFormattedCity
    public function getFormattedCity(?User $user) {
        // Null-safe chaining with method that has internal calls
        return $user?->getProfile()?->getAddress()?->getFormattedCity();
    }

    // CALL: User::getProfile
    // CALL: Profile::getAddress
    // CALL: Address::getCountry
    // CALL: strtolower
    public function getUserCountryCode(?User $user) {
        $country = $user?->getProfile()?->getAddress()?->getCountry();
        return $country ? strtolower($country) : 'unknown';
    }
}

class ReportGenerator {
    private $userService;

    public function __construct(UserService $service) {
        $this->userService = $service;
    }

    // CALL: UserService::getUserName
    // CALL: UserService::getUserCity
    // CALL: UserService::getUserCountryCode
    public function generateUserReport(?User $user) {
        return [
            'name' => $this->userService?->getUserName($user),
            'city' => $this->userService?->getUserCity($user),
            'country' => $this->userService?->getUserCountryCode($user),
        ];
    }

    // CALL: User::getProfile
    // CALL: Profile::getAddress
    public function hasCompleteProfile(?User $user) {
        $address = $user?->getProfile()?->getAddress();
        return $address !== null;
    }
}
