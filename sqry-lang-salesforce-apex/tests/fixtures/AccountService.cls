/**
 * Account service with SOQL queries
 */
public class AccountService {

    /**
     * Get accounts by industry
     */
    public static List<Account> getAccountsByIndustry(String industry) {
        return [SELECT Id, Name, Industry, AnnualRevenue
                FROM Account
                WHERE Industry = :industry];
    }

    /**
     * Get account with contacts
     */
    public static Account getAccountWithContacts(Id accountId) {
        Account acc = [SELECT Id, Name,
                       (SELECT Id, FirstName, LastName FROM Contacts)
                       FROM Account
                       WHERE Id = :accountId];
        return acc;
    }

    /**
     * Search accounts by name
     */
    public static List<Account> searchAccounts(String searchTerm) {
        String query = '%' + searchTerm + '%';
        List<Account> accounts = [SELECT Id, Name FROM Account WHERE Name LIKE :query];
        return accounts;
    }

    /**
     * Get opportunities for account
     */
    public static List<Opportunity> getAccountOpportunities(Id accountId) {
        return [SELECT Id, Name, StageName, Amount
                FROM Opportunity
                WHERE AccountId = :accountId];
    }
}
