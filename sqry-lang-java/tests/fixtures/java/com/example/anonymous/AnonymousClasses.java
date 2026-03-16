// Test fixture: Anonymous classes
// Tests: Anonymous class instantiation, method calls within anonymous classes

package com.example.anonymous;

public class AnonymousClasses {

    interface TaskRunner {
        void run();
    }

    static abstract class Worker {
        protected String name;

        public Worker(String name) {
            this.name = name;
        }

        public abstract void work();

        public String getName() {
            return name;
        }
    }

    public static void execute(TaskRunner r) {
        r.run();
    }

    public static void main(String[] args) {
        // Anonymous class implementing interface
        TaskRunner task1 = new TaskRunner() {
            @Override
            public void run() {
                System.out.println("Task 1 running");
                helper();
            }

            private void helper() {
                System.out.println("Helper called");
            }
        };

        task1.run();
        execute(task1);

        // Anonymous class extending abstract class
        Worker worker = new Worker("John") {
            @Override
            public void work() {
                String n = getName();
                System.out.println(n + " is working");
            }
        };

        worker.work();
    }
}
