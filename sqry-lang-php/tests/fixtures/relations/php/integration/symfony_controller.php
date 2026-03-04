<?php
/**
 * Symfony Controller Integration Fixture
 *
 * Represents a realistic Symfony application with:
 * - Controllers with dependency injection
 * - Services and repositories
 * - Form handling and validation
 * - Event dispatchers
 * - Doctrine ORM operations
 * - Response handling
 * - Security and authentication
 *
 * Ground truth annotations mark expected call and export edges.
 */

namespace App\Controller;

use App\Entity\User;
use App\Entity\Post;
use App\Form\UserType;
use App\Repository\UserRepository;
use App\Repository\PostRepository;
use App\Service\UserService;
use App\Service\EmailService;
use App\Service\CacheService;
use App\Event\UserRegisteredEvent;
use App\Event\PostCreatedEvent;
use Doctrine\ORM\EntityManagerInterface;
use Symfony\Bundle\FrameworkBundle\Controller\AbstractController;
use Symfony\Component\HttpFoundation\Request;
use Symfony\Component\HttpFoundation\Response;
use Symfony\Component\HttpFoundation\JsonResponse;
use Symfony\Component\Routing\Annotation\Route;
use Symfony\Component\EventDispatcher\EventDispatcherInterface;
use Symfony\Component\Security\Http\Attribute\IsGranted;
use Symfony\Component\Validator\Validator\ValidatorInterface;
use Symfony\Component\PasswordHasher\Hasher\UserPasswordHasherInterface;
use Psr\Log\LoggerInterface;

// EXPORT: UserController
#[Route('/users')]
class UserController extends AbstractController
{
    private $userService;
    private $emailService;
    private $cacheService;
    private $logger;

    // EXPORT: __construct
    public function __construct(
        UserService $userService,
        EmailService $emailService,
        CacheService $cacheService,
        LoggerInterface $logger
    ) {
        $this->userService = $userService;
        $this->emailService = $emailService;
        $this->cacheService = $cacheService;
        $this->logger = $logger;
    }

    // CALL: UserService::getAllUsers
    // CALL: CacheService::get
    // CALL: CacheService::set
    // CALL: AbstractController::render
    // EXPORT: index
    #[Route('/', name: 'user_index', methods: ['GET'])]
    public function index(): Response
    {
        $cacheKey = 'users.all';

        $users = $this->cacheService->get($cacheKey, function () {
            return $this->userService->getAllUsers();
        });

        if (!$users) {
            $users = $this->userService->getAllUsers();
            $this->cacheService->set($cacheKey, $users, 3600);
        }

        return $this->render('user/index.html.twig', [
            'users' => $users,
        ]);
    }

    // CALL: UserService::getUserById
    // CALL: AbstractController::createNotFoundException
    // CALL: AbstractController::render
    // EXPORT: show
    #[Route('/{id}', name: 'user_show', methods: ['GET'])]
    public function show(int $id): Response
    {
        $user = $this->userService->getUserById($id);

        if (!$user) {
            throw $this->createNotFoundException('User not found');
        }

        return $this->render('user/show.html.twig', [
            'user' => $user,
        ]);
    }

    // CALL: AbstractController::createForm
    // CALL: Request::isMethod
    // CALL: Form::handleRequest
    // CALL: Form::isSubmitted
    // CALL: Form::isValid
    // CALL: UserService::createUser
    // CALL: AbstractController::addFlash
    // CALL: AbstractController::redirectToRoute
    // CALL: AbstractController::render
    // EXPORT: create
    #[Route('/new', name: 'user_create', methods: ['GET', 'POST'])]
    #[IsGranted('ROLE_ADMIN')]
    public function create(Request $request): Response
    {
        $user = new User();
        $form = $this->createForm(UserType::class, $user);

        $form->handleRequest($request);

        if ($form->isSubmitted() && $form->isValid()) {
            $this->userService->createUser($user);

            $this->addFlash('success', 'User created successfully');

            return $this->redirectToRoute('user_index');
        }

        return $this->render('user/create.html.twig', [
            'form' => $form->createView(),
        ]);
    }

    // CALL: UserService::getUserById
    // CALL: AbstractController::createNotFoundException
    // CALL: AbstractController::createForm
    // CALL: Form::handleRequest
    // CALL: Form::isSubmitted
    // CALL: Form::isValid
    // CALL: UserService::updateUser
    // CALL: CacheService::delete
    // CALL: AbstractController::addFlash
    // CALL: AbstractController::redirectToRoute
    // CALL: AbstractController::render
    // EXPORT: edit
    #[Route('/{id}/edit', name: 'user_edit', methods: ['GET', 'POST'])]
    #[IsGranted('ROLE_ADMIN')]
    public function edit(Request $request, int $id): Response
    {
        $user = $this->userService->getUserById($id);

        if (!$user) {
            throw $this->createNotFoundException('User not found');
        }

        $form = $this->createForm(UserType::class, $user);
        $form->handleRequest($request);

        if ($form->isSubmitted() && $form->isValid()) {
            $this->userService->updateUser($user);

            $this->cacheService->delete("user.{$id}");
            $this->cacheService->delete('users.all');

            $this->addFlash('success', 'User updated successfully');

            return $this->redirectToRoute('user_show', ['id' => $id]);
        }

        return $this->render('user/edit.html.twig', [
            'user' => $user,
            'form' => $form->createView(),
        ]);
    }

    // CALL: UserService::getUserById
    // CALL: AbstractController::createNotFoundException
    // CALL: Request::isMethod
    // CALL: AbstractController::isCsrfTokenValid
    // CALL: UserService::deleteUser
    // CALL: CacheService::delete
    // CALL: AbstractController::addFlash
    // CALL: AbstractController::redirectToRoute
    // EXPORT: delete
    #[Route('/{id}', name: 'user_delete', methods: ['POST'])]
    #[IsGranted('ROLE_ADMIN')]
    public function delete(Request $request, int $id): Response
    {
        $user = $this->userService->getUserById($id);

        if (!$user) {
            throw $this->createNotFoundException('User not found');
        }

        if ($this->isCsrfTokenValid('delete' . $user->getId(), $request->request->get('_token'))) {
            $this->userService->deleteUser($user);

            $this->cacheService->delete("user.{$id}");
            $this->cacheService->delete('users.all');

            $this->addFlash('success', 'User deleted successfully');
        }

        return $this->redirectToRoute('user_index');
    }

    // CALL: Request::query
    // CALL: ParameterBag::get
    // CALL: UserService::searchUsers
    // CALL: JsonResponse::__construct
    // EXPORT: search
    #[Route('/api/search', name: 'user_search', methods: ['GET'])]
    public function search(Request $request): JsonResponse
    {
        $query = $request->query->get('q', '');
        $limit = $request->query->get('limit', 20);

        $results = $this->userService->searchUsers($query, $limit);

        return new JsonResponse([
            'success' => true,
            'data' => $results,
            'count' => count($results),
        ]);
    }

    // CALL: Request::toArray
    // CALL: UserService::validateRegistrationData
    // CALL: UserService::registerUser
    // CALL: EmailService::sendWelcomeEmail
    // CALL: JsonResponse::__construct
    // CALL: LoggerInterface::error
    // CALL: JsonResponse::__construct
    // EXPORT: register
    #[Route('/api/register', name: 'user_register', methods: ['POST'])]
    public function register(Request $request): JsonResponse
    {
        try {
            $data = $request->toArray();

            $errors = $this->userService->validateRegistrationData($data);

            if (!empty($errors)) {
                return new JsonResponse([
                    'success' => false,
                    'errors' => $errors,
                ], 400);
            }

            $user = $this->userService->registerUser($data);

            $this->emailService->sendWelcomeEmail($user);

            return new JsonResponse([
                'success' => true,
                'data' => [
                    'id' => $user->getId(),
                    'email' => $user->getEmail(),
                ],
            ], 201);

        } catch (\Exception $e) {
            $this->logger->error('Registration failed: ' . $e->getMessage());

            return new JsonResponse([
                'success' => false,
                'message' => 'Registration failed',
            ], 500);
        }
    }
}

// EXPORT: PostController
#[Route('/posts')]
class PostController extends AbstractController
{
    private $postRepository;
    private $entityManager;
    private $eventDispatcher;
    private $validator;

    // EXPORT: __construct
    public function __construct(
        PostRepository $postRepository,
        EntityManagerInterface $entityManager,
        EventDispatcherInterface $eventDispatcher,
        ValidatorInterface $validator
    ) {
        $this->postRepository = $postRepository;
        $this->entityManager = $entityManager;
        $this->eventDispatcher = $eventDispatcher;
        $this->validator = $validator;
    }

    // CALL: Request::query
    // CALL: ParameterBag::get
    // CALL: PostRepository::findPaginated
    // CALL: AbstractController::render
    // EXPORT: index
    #[Route('/', name: 'post_index', methods: ['GET'])]
    public function index(Request $request): Response
    {
        $page = $request->query->get('page', 1);
        $limit = $request->query->get('limit', 20);

        $posts = $this->postRepository->findPaginated($page, $limit);

        return $this->render('post/index.html.twig', [
            'posts' => $posts,
            'page' => $page,
        ]);
    }

    // CALL: PostRepository::find
    // CALL: AbstractController::createNotFoundException
    // CALL: AbstractController::render
    // EXPORT: show
    #[Route('/{id}', name: 'post_show', methods: ['GET'])]
    public function show(int $id): Response
    {
        $post = $this->postRepository->find($id);

        if (!$post) {
            throw $this->createNotFoundException('Post not found');
        }

        return $this->render('post/show.html.twig', [
            'post' => $post,
        ]);
    }

    // CALL: Request::toArray
    // CALL: Post::__construct
    // CALL: Post::setTitle
    // CALL: Post::setContent
    // CALL: Post::setAuthor
    // CALL: AbstractController::getUser
    // CALL: ValidatorInterface::validate
    // CALL: EntityManagerInterface::persist
    // CALL: EntityManagerInterface::flush
    // CALL: EventDispatcherInterface::dispatch
    // CALL: JsonResponse::__construct
    // EXPORT: create
    #[Route('/api/posts', name: 'post_create_api', methods: ['POST'])]
    #[IsGranted('ROLE_USER')]
    public function create(Request $request): JsonResponse
    {
        $data = $request->toArray();

        $post = new Post();
        $post->setTitle($data['title'] ?? '');
        $post->setContent($data['content'] ?? '');
        $post->setAuthor($this->getUser());

        $errors = $this->validator->validate($post);

        if (count($errors) > 0) {
            $errorMessages = [];
            foreach ($errors as $error) {
                $errorMessages[] = $error->getMessage();
            }

            return new JsonResponse([
                'success' => false,
                'errors' => $errorMessages,
            ], 400);
        }

        $this->entityManager->persist($post);
        $this->entityManager->flush();

        $this->eventDispatcher->dispatch(new PostCreatedEvent($post));

        return new JsonResponse([
            'success' => true,
            'data' => [
                'id' => $post->getId(),
                'title' => $post->getTitle(),
            ],
        ], 201);
    }

    // CALL: PostRepository::find
    // CALL: AbstractController::createNotFoundException
    // CALL: Request::toArray
    // CALL: Post::setTitle
    // CALL: Post::setContent
    // CALL: ValidatorInterface::validate
    // CALL: EntityManagerInterface::flush
    // CALL: JsonResponse::__construct
    // EXPORT: update
    #[Route('/api/posts/{id}', name: 'post_update_api', methods: ['PUT'])]
    #[IsGranted('ROLE_USER')]
    public function update(Request $request, int $id): JsonResponse
    {
        $post = $this->postRepository->find($id);

        if (!$post) {
            throw $this->createNotFoundException('Post not found');
        }

        $data = $request->toArray();

        if (isset($data['title'])) {
            $post->setTitle($data['title']);
        }

        if (isset($data['content'])) {
            $post->setContent($data['content']);
        }

        $errors = $this->validator->validate($post);

        if (count($errors) > 0) {
            $errorMessages = [];
            foreach ($errors as $error) {
                $errorMessages[] = $error->getMessage();
            }

            return new JsonResponse([
                'success' => false,
                'errors' => $errorMessages,
            ], 400);
        }

        $this->entityManager->flush();

        return new JsonResponse([
            'success' => true,
            'data' => [
                'id' => $post->getId(),
                'title' => $post->getTitle(),
            ],
        ]);
    }

    // CALL: PostRepository::find
    // CALL: AbstractController::createNotFoundException
    // CALL: EntityManagerInterface::remove
    // CALL: EntityManagerInterface::flush
    // CALL: JsonResponse::__construct
    // EXPORT: delete
    #[Route('/api/posts/{id}', name: 'post_delete_api', methods: ['DELETE'])]
    #[IsGranted('ROLE_USER')]
    public function delete(int $id): JsonResponse
    {
        $post = $this->postRepository->find($id);

        if (!$post) {
            throw $this->createNotFoundException('Post not found');
        }

        $this->entityManager->remove($post);
        $this->entityManager->flush();

        return new JsonResponse([
            'success' => true,
            'message' => 'Post deleted successfully',
        ]);
    }

    // CALL: PostRepository::findPublishedPosts
    // CALL: JsonResponse::__construct
    // EXPORT: published
    #[Route('/api/posts/published', name: 'post_published_api', methods: ['GET'])]
    public function published(): JsonResponse
    {
        $posts = $this->postRepository->findPublishedPosts();

        return new JsonResponse([
            'success' => true,
            'data' => array_map(function ($post) {
                return [
                    'id' => $post->getId(),
                    'title' => $post->getTitle(),
                    'author' => $post->getAuthor()->getEmail(),
                    'published_at' => $post->getPublishedAt()->format('Y-m-d H:i:s'),
                ];
            }, $posts),
        ]);
    }
}

namespace App\Service;

use App\Entity\User;
use App\Repository\UserRepository;
use App\Event\UserRegisteredEvent;
use Doctrine\ORM\EntityManagerInterface;
use Symfony\Component\EventDispatcher\EventDispatcherInterface;
use Symfony\Component\PasswordHasher\Hasher\UserPasswordHasherInterface;
use Symfony\Component\Validator\Validator\ValidatorInterface;

// EXPORT: UserService
class UserService
{
    private $userRepository;
    private $entityManager;
    private $eventDispatcher;
    private $passwordHasher;
    private $validator;

    // EXPORT: __construct
    public function __construct(
        UserRepository $userRepository,
        EntityManagerInterface $entityManager,
        EventDispatcherInterface $eventDispatcher,
        UserPasswordHasherInterface $passwordHasher,
        ValidatorInterface $validator
    ) {
        $this->userRepository = $userRepository;
        $this->entityManager = $entityManager;
        $this->eventDispatcher = $eventDispatcher;
        $this->passwordHasher = $passwordHasher;
        $this->validator = $validator;
    }

    // CALL: UserRepository::findAll
    // EXPORT: getAllUsers
    public function getAllUsers(): array
    {
        return $this->userRepository->findAll();
    }

    // CALL: UserRepository::find
    // EXPORT: getUserById
    public function getUserById(int $id): ?User
    {
        return $this->userRepository->find($id);
    }

    // CALL: EntityManagerInterface::persist
    // CALL: EntityManagerInterface::flush
    // CALL: EventDispatcherInterface::dispatch
    // EXPORT: createUser
    public function createUser(User $user): void
    {
        $this->entityManager->persist($user);
        $this->entityManager->flush();

        $this->eventDispatcher->dispatch(new UserRegisteredEvent($user));
    }

    // CALL: EntityManagerInterface::flush
    // EXPORT: updateUser
    public function updateUser(User $user): void
    {
        $this->entityManager->flush();
    }

    // CALL: EntityManagerInterface::remove
    // CALL: EntityManagerInterface::flush
    // EXPORT: deleteUser
    public function deleteUser(User $user): void
    {
        $this->entityManager->remove($user);
        $this->entityManager->flush();
    }

    // CALL: UserRepository::searchByEmailOrName
    // EXPORT: searchUsers
    public function searchUsers(string $query, int $limit = 20): array
    {
        return $this->userRepository->searchByEmailOrName($query, $limit);
    }

    // CALL: ValidatorInterface::validate
    // EXPORT: validateRegistrationData
    public function validateRegistrationData(array $data): array
    {
        $errors = [];

        if (empty($data['email'])) {
            $errors['email'] = 'Email is required';
        }

        if (empty($data['password'])) {
            $errors['password'] = 'Password is required';
        }

        return $errors;
    }

    // CALL: User::__construct
    // CALL: User::setEmail
    // CALL: UserPasswordHasherInterface::hashPassword
    // CALL: User::setPassword
    // CALL: EntityManagerInterface::persist
    // CALL: EntityManagerInterface::flush
    // CALL: EventDispatcherInterface::dispatch
    // EXPORT: registerUser
    public function registerUser(array $data): User
    {
        $user = new User();
        $user->setEmail($data['email']);

        $hashedPassword = $this->passwordHasher->hashPassword($user, $data['password']);
        $user->setPassword($hashedPassword);

        $this->entityManager->persist($user);
        $this->entityManager->flush();

        $this->eventDispatcher->dispatch(new UserRegisteredEvent($user));

        return $user;
    }
}

// EXPORT: EmailService
class EmailService
{
    // CALL: sprintf
    // EXPORT: sendWelcomeEmail
    public function sendWelcomeEmail(User $user): void
    {
        $subject = 'Welcome to our platform';
        $message = sprintf('Hello %s, welcome to our platform!', $user->getEmail());

        // Send email logic here
    }

    // CALL: sprintf
    // EXPORT: sendPasswordResetEmail
    public function sendPasswordResetEmail(User $user, string $token): void
    {
        $subject = 'Password Reset Request';
        $message = sprintf('Click here to reset your password: %s', $token);

        // Send email logic here
    }
}

// EXPORT: CacheService
class CacheService
{
    private $cache = [];

    // EXPORT: get
    public function get(string $key, ?callable $callback = null)
    {
        if (isset($this->cache[$key])) {
            return $this->cache[$key];
        }

        if ($callback) {
            $value = $callback();
            $this->set($key, $value);
            return $value;
        }

        return null;
    }

    // EXPORT: set
    public function set(string $key, $value, int $ttl = 3600): void
    {
        $this->cache[$key] = $value;
    }

    // EXPORT: delete
    public function delete(string $key): void
    {
        unset($this->cache[$key]);
    }

    // EXPORT: clear
    public function clear(): void
    {
        $this->cache = [];
    }
}
