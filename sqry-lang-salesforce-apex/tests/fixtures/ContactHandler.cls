/**
 * Contact handler with DML operations
 */
public class ContactHandler {

    /**
     * Create contacts for accounts
     */
    public static void createContacts(List<Account> accounts) {
        List<Contact> contacts = new List<Contact>();

        for (Account acc : accounts) {
            Contact c = new Contact(
                FirstName = 'Primary',
                LastName = 'Contact',
                AccountId = acc.Id
            );
            contacts.add(c);
        }

        insert contacts;
    }

    /**
     * Update contact email addresses
     */
    public static void updateContactEmails(List<Contact> contacts, String domain) {
        for (Contact c : contacts) {
            c.Email = c.FirstName + '.' + c.LastName + '@' + domain;
        }

        update contacts;
    }

    /**
     * Delete contacts without accounts
     */
    public static void deleteOrphanedContacts() {
        List<Contact> orphaned = [SELECT Id FROM Contact WHERE AccountId = null];

        delete orphaned;
    }

    /**
     * Upsert contacts by email
     */
    public static void upsertContactsByEmail(List<Contact> contacts) {
        upsert contacts Email;
    }

    /**
     * Undelete recently deleted contacts
     */
    public static void restoreContacts(List<Id> contactIds) {
        List<Contact> deleted = [SELECT Id FROM Contact WHERE Id IN :contactIds ALL ROWS];
        undelete deleted;
    }
}
