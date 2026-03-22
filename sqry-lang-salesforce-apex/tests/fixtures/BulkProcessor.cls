/**
 * Bulk processor demonstrating SOQL/DML in loops (anti-pattern for detection)
 * These patterns are tagged for semantic search, not risk scoring
 */
public class BulkProcessor {

    /**
     * SOQL in loop example (anti-pattern)
     */
    public static void processAccountsWithSoqlInLoop(List<Id> accountIds) {
        for (Id accountId : accountIds) {
            // SOQL in loop - should be tagged with soql_in_loop=true
            Account acc = [SELECT Id, Name FROM Account WHERE Id = :accountId];
            System.debug('Processing: ' + acc.Name);
        }
    }

    /**
     * DML in loop example (anti-pattern)
     */
    public static void updateContactsWithDmlInLoop(Map<Id, String> contactUpdates) {
        for (Id contactId : contactUpdates.keySet()) {
            Contact c = new Contact(Id = contactId, Description = contactUpdates.get(contactId));
            // DML in loop - should be tagged with dml_in_loop=true
            update c;
        }
    }

    /**
     * Nested loops with SOQL (anti-pattern)
     */
    public static void processNestedLoops(List<Account> accounts) {
        for (Account acc : accounts) {
            for (Integer i = 0; i < 10; i++) {
                // SOQL in nested loop
                List<Contact> contacts = [SELECT Id FROM Contact WHERE AccountId = :acc.Id];
                System.debug('Found ' + contacts.size() + ' contacts');
            }
        }
    }

    /**
     * Correct pattern - SOQL outside loop
     */
    public static void processAccountsCorrectly(List<Id> accountIds) {
        // SOQL outside loop - should NOT be tagged with soql_in_loop
        List<Account> accounts = [SELECT Id, Name FROM Account WHERE Id IN :accountIds];

        for (Account acc : accounts) {
            System.debug('Processing: ' + acc.Name);
        }
    }

    /**
     * Correct pattern - DML outside loop
     */
    public static void updateContactsCorrectly(Map<Id, String> contactUpdates) {
        List<Contact> contacts = new List<Contact>();

        for (Id contactId : contactUpdates.keySet()) {
            contacts.add(new Contact(Id = contactId, Description = contactUpdates.get(contactId)));
        }

        // DML outside loop - should NOT be tagged with dml_in_loop
        update contacts;
    }
}
