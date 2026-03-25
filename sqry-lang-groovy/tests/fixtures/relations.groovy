import groovy.transform.CompileStatic

@CompileStatic
trait Auditable {
    void audit(String message) {
        println "[AUDIT] $message"
    }
}

@CompileStatic
abstract class BaseService {
    abstract void execute()
}

@CompileStatic
class ServiceTask extends BaseService implements Auditable {
    final Closure<Integer> helper
    final String name

    ServiceTask(String name) {
        this.name = name
        this.helper = { int value -> value * 2 }
    }

    @Override
    void execute() {
        audit("Executing ${name}")
    }

    void run(def task) {
        task.dependsOn 'compileJava'
        def computed = helper(task.name.length())
        audit("Computed ${computed} for ${task.name}")
        execute()
    }
}

def helperClosure = { String input -> input.toUpperCase() }

task compileJava {
    doLast {
        println "Compiling sources for ${project.name}"
    }
}

def projectName = 'example-app'

task runService {
    dependsOn compileJava
    doLast {
        def task = new ServiceTask(projectName)
        task.run(it)
        println "Helper says ${helperClosure(projectName)}"
    }
}

dependencies {
    implementation 'org.apache.commons:commons-lang3:3.14.0'
    testImplementation "org.spockframework:spock-core:${spockVersion}"
}
