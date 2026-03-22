/**
 * Account trigger demonstrating all trigger events
 */
trigger AccountTrigger on Account (before insert, before update, before delete,
                                   after insert, after update, after delete, after undelete) {

    // Before insert logic
    if (Trigger.isBefore && Trigger.isInsert) {
        for (Account acc : Trigger.new) {
            if (acc.Name == null) {
                acc.Name = 'Default Account';
            }
        }
    }

    // After insert logic
    if (Trigger.isAfter && Trigger.isInsert) {
        List<Contact> contacts = new List<Contact>();
        for (Account acc : Trigger.new) {
            Contact c = new Contact(
                FirstName = 'Primary',
                LastName = 'Contact',
                AccountId = acc.Id
            );
            contacts.add(c);
        }
        insert contacts;
    }

    // After update logic
    if (Trigger.isAfter && Trigger.isUpdate) {
        Set<Id> accountIds = new Set<Id>();
        for (Account acc : Trigger.new) {
            if (acc.AnnualRevenue != Trigger.oldMap.get(acc.Id).AnnualRevenue) {
                accountIds.add(acc.Id);
            }
        }
    }
}
