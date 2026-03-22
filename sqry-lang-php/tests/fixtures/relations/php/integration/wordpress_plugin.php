<?php
/**
 * WordPress Plugin Integration Fixture
 *
 * Represents a realistic WordPress plugin implementation with:
 * - WordPress hooks (actions and filters)
 * - Custom post types
 * - Meta boxes
 * - Settings API
 * - AJAX handlers
 * - Database operations
 * - Shortcodes
 *
 * Ground truth annotations mark expected call and export edges.
 */

/*
Plugin Name: Advanced User Manager
Plugin URI: https://example.com/advanced-user-manager
Description: Comprehensive user management plugin for WordPress
Version: 1.0.0
Author: Example Developer
*/

namespace AdvancedUserManager;

// EXPORT: Plugin
class Plugin
{
    private static $instance = null;
    private $settings;
    private $userService;
    private $metaBoxManager;

    // EXPORT: getInstance
    public static function getInstance()
    {
        if (self::$instance === null) {
            self::$instance = new self();
        }
        return self::$instance;
    }

    // CALL: Settings::__construct
    // CALL: UserService::__construct
    // CALL: MetaBoxManager::__construct
    // NOT EXPORTED: private constructor
    private function __construct()
    {
        $this->settings = new Settings();
        $this->userService = new UserService();
        $this->metaBoxManager = new MetaBoxManager();
    }

    // CALL: add_action
    // CALL: add_action (multiple times)
    // CALL: add_filter
    // CALL: add_shortcode
    // EXPORT: init
    public function init()
    {
        // Register hooks
        add_action('init', [$this, 'registerPostTypes']);
        add_action('admin_menu', [$this, 'registerAdminMenus']);
        add_action('admin_enqueue_scripts', [$this, 'enqueueAdminAssets']);
        add_action('wp_ajax_aum_get_users', [$this, 'handleGetUsersAjax']);
        add_action('wp_ajax_nopriv_aum_get_users', [$this, 'handleGetUsersAjax']);
        add_action('user_register', [$this, 'onUserRegister']);
        add_action('profile_update', [$this, 'onProfileUpdate']);

        add_filter('manage_users_columns', [$this, 'addUserColumns']);
        add_filter('manage_users_custom_column', [$this, 'renderUserColumn'], 10, 3);

        add_shortcode('aum_user_profile', [$this, 'renderUserProfileShortcode']);

        // Initialize sub-components
        $this->settings->init();
        $this->metaBoxManager->init();
    }

    // CALL: register_post_type
    // CALL: __
    // EXPORT: registerPostTypes
    public function registerPostTypes()
    {
        register_post_type('aum_user_group', [
            'labels' => [
                'name' => __('User Groups', 'advanced-user-manager'),
                'singular_name' => __('User Group', 'advanced-user-manager'),
            ],
            'public' => true,
            'has_archive' => true,
            'supports' => ['title', 'editor'],
        ]);
    }

    // CALL: add_menu_page
    // CALL: add_submenu_page
    // CALL: __
    // EXPORT: registerAdminMenus
    public function registerAdminMenus()
    {
        add_menu_page(
            __('User Manager', 'advanced-user-manager'),
            __('User Manager', 'advanced-user-manager'),
            'manage_options',
            'aum-dashboard',
            [$this, 'renderDashboard'],
            'dashicons-groups'
        );

        add_submenu_page(
            'aum-dashboard',
            __('Settings', 'advanced-user-manager'),
            __('Settings', 'advanced-user-manager'),
            'manage_options',
            'aum-settings',
            [$this->settings, 'render']
        );
    }

    // CALL: wp_enqueue_script
    // CALL: wp_enqueue_style
    // CALL: plugins_url
    // CALL: wp_localize_script
    // CALL: wp_create_nonce
    // EXPORT: enqueueAdminAssets
    public function enqueueAdminAssets($hook)
    {
        if (!in_array($hook, ['toplevel_page_aum-dashboard', 'user-manager_page_aum-settings'])) {
            return;
        }

        wp_enqueue_script(
            'aum-admin',
            plugins_url('assets/js/admin.js', __FILE__),
            ['jquery'],
            '1.0.0',
            true
        );

        wp_enqueue_style(
            'aum-admin',
            plugins_url('assets/css/admin.css', __FILE__),
            [],
            '1.0.0'
        );

        wp_localize_script('aum-admin', 'aumData', [
            'ajaxUrl' => admin_url('admin-ajax.php'),
            'nonce' => wp_create_nonce('aum_nonce'),
        ]);
    }

    // CALL: check_ajax_referer
    // CALL: current_user_can
    // CALL: wp_send_json_error
    // CALL: UserService::getUsers
    // CALL: wp_send_json_success
    // EXPORT: handleGetUsersAjax
    public function handleGetUsersAjax()
    {
        check_ajax_referer('aum_nonce', 'nonce');

        if (!current_user_can('manage_options')) {
            wp_send_json_error(['message' => 'Unauthorized']);
            return;
        }

        $page = isset($_POST['page']) ? intval($_POST['page']) : 1;
        $perPage = isset($_POST['per_page']) ? intval($_POST['per_page']) : 20;

        $users = $this->userService->getUsers($page, $perPage);

        wp_send_json_success($users);
    }

    // CALL: UserService::onUserCreated
    // EXPORT: onUserRegister
    public function onUserRegister($userId)
    {
        $this->userService->onUserCreated($userId);
    }

    // CALL: UserService::onUserUpdated
    // EXPORT: onProfileUpdate
    public function onProfileUpdate($userId)
    {
        $this->userService->onUserUpdated($userId);
    }

    // CALL: __
    // EXPORT: addUserColumns
    public function addUserColumns($columns)
    {
        $columns['aum_user_group'] = __('User Group', 'advanced-user-manager');
        $columns['aum_last_login'] = __('Last Login', 'advanced-user-manager');
        return $columns;
    }

    // CALL: get_user_meta
    // CALL: date
    // CALL: esc_html
    // EXPORT: renderUserColumn
    public function renderUserColumn($value, $columnName, $userId)
    {
        switch ($columnName) {
            case 'aum_user_group':
                $groupId = get_user_meta($userId, 'aum_user_group', true);
                return $groupId ? esc_html(get_the_title($groupId)) : '—';

            case 'aum_last_login':
                $lastLogin = get_user_meta($userId, 'aum_last_login', true);
                return $lastLogin ? esc_html(date('Y-m-d H:i:s', $lastLogin)) : 'Never';

            default:
                return $value;
        }
    }

    // CALL: UserService::getUserProfile
    // CALL: Plugin::renderTemplate
    // EXPORT: renderUserProfileShortcode
    public function renderUserProfileShortcode($atts)
    {
        $atts = shortcode_atts([
            'user_id' => get_current_user_id(),
        ], $atts);

        $profile = $this->userService->getUserProfile($atts['user_id']);

        return $this->renderTemplate('user-profile', ['profile' => $profile]);
    }

    // CALL: locate_template
    // CALL: load_template
    // NOT EXPORTED: private
    private function renderTemplate($name, $args = [])
    {
        $templatePath = locate_template("aum-templates/{$name}.php");

        if (!$templatePath) {
            $templatePath = plugin_dir_path(__FILE__) . "templates/{$name}.php";
        }

        extract($args);
        ob_start();
        load_template($templatePath, false);
        return ob_get_clean();
    }

    // CALL: UserService::getDashboardStats
    // CALL: Plugin::renderTemplate
    // EXPORT: renderDashboard
    public function renderDashboard()
    {
        $stats = $this->userService->getDashboardStats();
        echo $this->renderTemplate('dashboard', ['stats' => $stats]);
    }
}

// EXPORT: UserService
class UserService
{
    // CALL: get_users
    // CALL: array_map
    // EXPORT: getUsers
    public function getUsers($page = 1, $perPage = 20)
    {
        $offset = ($page - 1) * $perPage;

        $users = get_users([
            'number' => $perPage,
            'offset' => $offset,
        ]);

        return array_map([$this, 'formatUser'], $users);
    }

    // CALL: get_user_meta
    // NOT EXPORTED: private
    private function formatUser($user)
    {
        return [
            'id' => $user->ID,
            'name' => $user->display_name,
            'email' => $user->user_email,
            'group' => get_user_meta($user->ID, 'aum_user_group', true),
            'last_login' => get_user_meta($user->ID, 'aum_last_login', true),
        ];
    }

    // CALL: get_userdata
    // CALL: get_user_meta
    // EXPORT: getUserProfile
    public function getUserProfile($userId)
    {
        $user = get_userdata($userId);

        if (!$user) {
            return null;
        }

        return [
            'id' => $user->ID,
            'name' => $user->display_name,
            'email' => $user->user_email,
            'registered' => $user->user_registered,
            'bio' => get_user_meta($userId, 'description', true),
            'avatar' => get_avatar_url($userId),
        ];
    }

    // CALL: update_user_meta
    // CALL: do_action
    // EXPORT: onUserCreated
    public function onUserCreated($userId)
    {
        update_user_meta($userId, 'aum_last_login', time());
        update_user_meta($userId, 'aum_registration_ip', $_SERVER['REMOTE_ADDR']);

        do_action('aum_user_created', $userId);
    }

    // CALL: update_user_meta
    // CALL: do_action
    // EXPORT: onUserUpdated
    public function onUserUpdated($userId)
    {
        update_user_meta($userId, 'aum_last_updated', time());

        do_action('aum_user_updated', $userId);
    }

    // CALL: count_users
    // CALL: get_users
    // CALL: get_user_meta
    // EXPORT: getDashboardStats
    public function getDashboardStats()
    {
        $userCounts = count_users();

        $recentUsers = get_users([
            'number' => 10,
            'orderby' => 'registered',
            'order' => 'DESC',
        ]);

        $activeToday = count(array_filter($recentUsers, function ($user) {
            $lastLogin = get_user_meta($user->ID, 'aum_last_login', true);
            return $lastLogin && $lastLogin > strtotime('today');
        }));

        return [
            'total_users' => $userCounts['total_users'],
            'recent_users' => count($recentUsers),
            'active_today' => $activeToday,
        ];
    }

    // CALL: wp_insert_user
    // CALL: wp_generate_password
    // CALL: is_wp_error
    // EXPORT: createUser
    public function createUser($username, $email, $role = 'subscriber')
    {
        $userData = [
            'user_login' => $username,
            'user_email' => $email,
            'user_pass' => wp_generate_password(),
            'role' => $role,
        ];

        $userId = wp_insert_user($userData);

        if (is_wp_error($userId)) {
            return false;
        }

        $this->onUserCreated($userId);

        return $userId;
    }

    // CALL: wp_update_user
    // CALL: is_wp_error
    // EXPORT: updateUser
    public function updateUser($userId, $data)
    {
        $data['ID'] = $userId;

        $result = wp_update_user($data);

        if (is_wp_error($result)) {
            return false;
        }

        $this->onUserUpdated($userId);

        return true;
    }

    // CALL: wp_delete_user
    // CALL: do_action
    // EXPORT: deleteUser
    public function deleteUser($userId, $reassignId = null)
    {
        $result = wp_delete_user($userId, $reassignId);

        if ($result) {
            do_action('aum_user_deleted', $userId);
        }

        return $result;
    }
}

// EXPORT: Settings
class Settings
{
    private $optionGroup = 'aum_settings';
    private $optionName = 'aum_options';

    // CALL: add_action
    // EXPORT: init
    public function init()
    {
        add_action('admin_init', [$this, 'registerSettings']);
    }

    // CALL: register_setting
    // CALL: add_settings_section
    // CALL: add_settings_field
    // CALL: __
    // EXPORT: registerSettings
    public function registerSettings()
    {
        register_setting($this->optionGroup, $this->optionName, [
            'sanitize_callback' => [$this, 'sanitizeSettings'],
        ]);

        add_settings_section(
            'aum_general_section',
            __('General Settings', 'advanced-user-manager'),
            [$this, 'renderGeneralSection'],
            $this->optionGroup
        );

        add_settings_field(
            'enable_last_login',
            __('Track Last Login', 'advanced-user-manager'),
            [$this, 'renderCheckboxField'],
            $this->optionGroup,
            'aum_general_section',
            ['name' => 'enable_last_login']
        );

        add_settings_field(
            'default_user_group',
            __('Default User Group', 'advanced-user-manager'),
            [$this, 'renderSelectField'],
            $this->optionGroup,
            'aum_general_section',
            ['name' => 'default_user_group']
        );
    }

    // CALL: esc_html
    // CALL: __
    // EXPORT: renderGeneralSection
    public function renderGeneralSection()
    {
        echo '<p>' . esc_html__('Configure general plugin settings.', 'advanced-user-manager') . '</p>';
    }

    // CALL: get_option
    // CALL: checked
    // EXPORT: renderCheckboxField
    public function renderCheckboxField($args)
    {
        $options = get_option($this->optionName);
        $value = isset($options[$args['name']]) ? $options[$args['name']] : false;

        printf(
            '<input type="checkbox" name="%s[%s]" value="1" %s />',
            $this->optionName,
            $args['name'],
            checked($value, true, false)
        );
    }

    // CALL: get_option
    // CALL: get_posts
    // CALL: selected
    // EXPORT: renderSelectField
    public function renderSelectField($args)
    {
        $options = get_option($this->optionName);
        $value = isset($options[$args['name']]) ? $options[$args['name']] : '';

        $groups = get_posts([
            'post_type' => 'aum_user_group',
            'posts_per_page' => -1,
        ]);

        echo '<select name="' . $this->optionName . '[' . $args['name'] . ']">';
        echo '<option value="">None</option>';

        foreach ($groups as $group) {
            printf(
                '<option value="%s" %s>%s</option>',
                $group->ID,
                selected($value, $group->ID, false),
                $group->post_title
            );
        }

        echo '</select>';
    }

    // CALL: sanitize_text_field
    // CALL: absint
    // NOT EXPORTED: private
    private function sanitizeSettings($input)
    {
        $sanitized = [];

        if (isset($input['enable_last_login'])) {
            $sanitized['enable_last_login'] = (bool) $input['enable_last_login'];
        }

        if (isset($input['default_user_group'])) {
            $sanitized['default_user_group'] = absint($input['default_user_group']);
        }

        return $sanitized;
    }

    // CALL: settings_fields
    // CALL: do_settings_sections
    // CALL: submit_button
    // EXPORT: render
    public function render()
    {
        echo '<div class="wrap">';
        echo '<h1>' . __('Advanced User Manager Settings', 'advanced-user-manager') . '</h1>';
        echo '<form method="post" action="options.php">';

        settings_fields($this->optionGroup);
        do_settings_sections($this->optionGroup);
        submit_button();

        echo '</form>';
        echo '</div>';
    }
}

// EXPORT: MetaBoxManager
class MetaBoxManager
{
    // CALL: add_action
    // EXPORT: init
    public function init()
    {
        add_action('add_meta_boxes', [$this, 'registerMetaBoxes']);
        add_action('save_post', [$this, 'saveMetaBoxes']);
    }

    // CALL: add_meta_box
    // CALL: __
    // EXPORT: registerMetaBoxes
    public function registerMetaBoxes()
    {
        add_meta_box(
            'aum_group_users',
            __('Group Users', 'advanced-user-manager'),
            [$this, 'renderGroupUsersMetaBox'],
            'aum_user_group',
            'normal',
            'default'
        );
    }

    // CALL: get_post_meta
    // CALL: get_users
    // CALL: wp_nonce_field
    // CALL: checked
    // EXPORT: renderGroupUsersMetaBox
    public function renderGroupUsersMetaBox($post)
    {
        $selectedUsers = get_post_meta($post->ID, '_aum_group_users', true) ?: [];
        $allUsers = get_users();

        wp_nonce_field('aum_group_users_nonce', 'aum_group_users_nonce');

        echo '<div class="aum-user-list">';
        foreach ($allUsers as $user) {
            printf(
                '<label><input type="checkbox" name="aum_group_users[]" value="%d" %s /> %s</label><br />',
                $user->ID,
                checked(in_array($user->ID, $selectedUsers), true, false),
                $user->display_name
            );
        }
        echo '</div>';
    }

    // CALL: verify_nonce
    // CALL: current_user_can
    // CALL: array_map
    // CALL: update_post_meta
    // EXPORT: saveMetaBoxes
    public function saveMetaBoxes($postId)
    {
        if (!isset($_POST['aum_group_users_nonce']) || !wp_verify_nonce($_POST['aum_group_users_nonce'], 'aum_group_users_nonce')) {
            return;
        }

        if (!current_user_can('edit_post', $postId)) {
            return;
        }

        if (isset($_POST['aum_group_users'])) {
            $userIds = array_map('intval', $_POST['aum_group_users']);
            update_post_meta($postId, '_aum_group_users', $userIds);
        } else {
            update_post_meta($postId, '_aum_group_users', []);
        }
    }
}

// Initialize plugin
// CALL: Plugin::getInstance
// CALL: Plugin::init
add_action('plugins_loaded', function () {
    Plugin::getInstance()->init();
});
