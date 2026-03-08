/**
 * API controller with various Apex annotations
 */
public class ApiController {

    /**
     * AuraEnabled method for Lightning components
     */
    @AuraEnabled
    public static List<Account> getAccounts() {
        return [SELECT Id, Name FROM Account LIMIT 10];
    }

    /**
     * Future method for async processing
     */
    @future
    public static void processAccountsAsync(Set<Id> accountIds) {
        List<Account> accounts = [SELECT Id, Name FROM Account WHERE Id IN :accountIds];
        // Process accounts
    }

    /**
     * Future method with callout
     */
    @future(callout=true)
    public static void sendAccountData(Id accountId) {
        Account acc = [SELECT Id, Name FROM Account WHERE Id = :accountId];
        // Make HTTP callout
    }

    /**
     * Invocable method for Process Builder/Flow
     */
    @InvocableMethod(label='Update Account Status' description='Updates account status based on criteria')
    public static void updateAccountStatus(List<Id> accountIds) {
        List<Account> accounts = [SELECT Id, Status__c FROM Account WHERE Id IN :accountIds];
        for (Account acc : accounts) {
            acc.Status__c = 'Processed';
        }
        update accounts;
    }

    /**
     * RemoteAction for Visualforce
     */
    @RemoteAction
    public static Account getAccount(Id accountId) {
        return [SELECT Id, Name, Industry FROM Account WHERE Id = :accountId];
    }

    /**
     * TestVisible method
     */
    @TestVisible
    private static String getSecretKey() {
        return 'test-key-12345';
    }
}
